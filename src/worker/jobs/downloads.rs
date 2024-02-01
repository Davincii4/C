mod message;

use crate::config::{CdnLogQueueConfig, CdnLogStorageConfig};
use crate::db::DieselPool;
use crate::sqs::{MockSqsQueue, SqsQueue, SqsQueueImpl};
use crate::tasks::spawn_blocking;
use crate::worker::Environment;
use anyhow::Context;
use aws_credential_types::Credentials;
use aws_sdk_sqs::config::Region;
use crates_io_cdn_logs::{count_downloads, Decompressor};
use crates_io_worker::BackgroundJob;
use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::memory::InMemory;
use object_store::ObjectStore;
use std::cmp::Reverse;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::BufReader;

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessCdnLog {
    region: String,
    bucket: String,
    path: String,
}

impl ProcessCdnLog {
    pub fn new(region: String, bucket: String, path: String) -> Self {
        Self {
            region,
            bucket,
            path,
        }
    }
}

impl BackgroundJob for ProcessCdnLog {
    const JOB_NAME: &'static str = "process_cdn_log";

    type Context = Arc<Environment>;

    async fn run(&self, ctx: Self::Context) -> anyhow::Result<()> {
        let store = self
            .build_store(&ctx.config.cdn_log_storage)
            .context("Failed to build object store")?;

        self.run(store).await
    }
}

impl ProcessCdnLog {
    fn build_store(&self, config: &CdnLogStorageConfig) -> anyhow::Result<Box<dyn ObjectStore>> {
        match config {
            CdnLogStorageConfig::S3 {
                access_key,
                secret_key,
            } => {
                use secrecy::ExposeSecret;

                let store = AmazonS3Builder::new()
                    .with_region(&self.region)
                    .with_bucket_name(&self.bucket)
                    .with_access_key_id(access_key)
                    .with_secret_access_key(secret_key.expose_secret())
                    .build()?;

                Ok(Box::new(store))
            }
            CdnLogStorageConfig::Local { path } => {
                Ok(Box::new(LocalFileSystem::new_with_prefix(path)?))
            }
            CdnLogStorageConfig::Memory => Ok(Box::new(InMemory::new())),
        }
    }

    async fn run(&self, store: Box<dyn ObjectStore>) -> anyhow::Result<()> {
        let path = object_store::path::Path::parse(&self.path)
            .with_context(|| format!("Failed to parse path: {:?}", self.path))?;

        let meta = store.head(&path).await;
        let meta = meta.with_context(|| format!("Failed to request metadata for {path:?}"))?;

        let reader = object_store::buffered::BufReader::new(Arc::new(store), &meta);
        let decompressor = Decompressor::from_extension(reader, path.extension())?;
        let reader = BufReader::new(decompressor);

        let parse_start = Instant::now();
        let downloads = count_downloads(reader).await?;
        let parse_duration = parse_start.elapsed();

        // TODO: for now this background job just prints out the results, but
        // eventually it should insert them into the database instead.

        if downloads.as_inner().is_empty() {
            info!("No downloads found in log file: {path}");
            return Ok(());
        }

        let num_crates = downloads
            .as_inner()
            .iter()
            .map(|((_, krate, _), _)| krate)
            .collect::<HashSet<_>>()
            .len();

        let total_inserts = downloads.as_inner().len();

        let total_downloads = downloads
            .as_inner()
            .iter()
            .map(|(_, downloads)| downloads)
            .sum::<u64>();

        info!("Log file: {path}");
        info!("Number of crates: {num_crates}");
        info!("Number of needed inserts: {total_inserts}");
        info!("Total number of downloads: {total_downloads}");
        info!("Time to parse: {parse_duration:?}");

        let mut downloads = downloads.into_inner().into_iter().collect::<Vec<_>>();
        downloads.sort_by_key(|((_, _, _), downloads)| Reverse(*downloads));

        let top_downloads = downloads
            .into_iter()
            .take(30)
            .map(|((krate, version, date), downloads)| {
                format!("{date}  {krate}@{version} .. {downloads}")
            })
            .collect::<Vec<_>>();

        info!("Top 30 downloads: {top_downloads:?}");

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, clap::Parser)]
pub struct ProcessCdnLogQueue {
    /// The maximum number of messages to receive from the queue and process.
    #[clap(long, default_value = "1")]
    max_messages: usize,
}

impl BackgroundJob for ProcessCdnLogQueue {
    const JOB_NAME: &'static str = "process_cdn_log_queue";

    type Context = Arc<Environment>;

    async fn run(&self, ctx: Self::Context) -> anyhow::Result<()> {
        let queue = Self::build_queue(&ctx.config.cdn_log_queue);
        self.run(queue, &ctx.connection_pool).await
    }
}

impl ProcessCdnLogQueue {
    fn build_queue(config: &CdnLogQueueConfig) -> Box<dyn SqsQueue + Send + Sync> {
        match config {
            CdnLogQueueConfig::Mock => Box::new(MockSqsQueue::new()),
            CdnLogQueueConfig::SQS {
                access_key,
                secret_key,
                region,
                queue_url,
            } => {
                use secrecy::ExposeSecret;

                let secret_key = secret_key.expose_secret();
                let credentials = Credentials::from_keys(access_key, secret_key, None);

                let region = Region::new(region.to_owned());

                Box::new(SqsQueueImpl::new(queue_url, region, credentials))
            }
        }
    }

    async fn run(
        &self,
        queue: Box<dyn SqsQueue + Send + Sync>,
        connection_pool: &DieselPool,
    ) -> anyhow::Result<()> {
        const MAX_BATCH_SIZE: usize = 10;

        info!("Receiving messages from the CDN log queue…");
        let mut num_remaining = self.max_messages;
        while num_remaining > 0 {
            let batch_size = num_remaining.min(MAX_BATCH_SIZE);
            num_remaining -= batch_size;

            debug!("Receiving next {batch_size} messages from the CDN log queue…");
            let response = queue.receive_messages(batch_size as i32).await?;

            let messages = response.messages();
            debug!(
                "Received {num_messages} messages from the CDN log queue",
                num_messages = messages.len()
            );
            if messages.is_empty() {
                info!("No more messages to receive from the CDN log queue");
                break;
            }

            for message in messages {
                let message_id = message.message_id().unwrap_or("<unknown>");
                debug!("Processing message: {message_id}");

                let Some(receipt_handle) = message.receipt_handle() else {
                    warn!("Message {message_id} has no receipt handle; skipping");
                    continue;
                };

                debug!("Deleting message {message_id} from the CDN log queue…");
                queue
                    .delete_message(receipt_handle)
                    .await
                    .with_context(|| {
                        format!("Failed to delete message {message_id} from the CDN log queue")
                    })?;

                let Some(body) = message.body() else {
                    warn!("Message {message_id} has no body; skipping");
                    continue;
                };

                let message = match serde_json::from_str::<message::Message>(body) {
                    Ok(message) => message,
                    Err(err) => {
                        warn!("Failed to parse message {message_id}: {err}");
                        continue;
                    }
                };

                if message.records.is_empty() {
                    warn!("Message {message_id} has no records; skipping");
                    continue;
                }

                let pool = connection_pool.clone();
                spawn_blocking({
                    let message_id = message_id.to_owned();
                    move || {
                        let mut conn = pool
                            .get()
                            .context("Failed to acquire database connection")?;

                        for record in message.records {
                            let region = record.aws_region;
                            let bucket = record.s3.bucket.name;
                            let path = record.s3.object.key;

                            if Self::is_ignored_path(&path) {
                                debug!("Skipping ignored path: {path}");
                                continue;
                            }

                            let path = match object_store::path::Path::from_url_path(&path) {
                                Ok(path) => path,
                                Err(err) => {
                                    warn!("Failed to parse path ({path}): {err}");
                                    continue;
                                }
                            };

                            info!("Enqueuing processing job for message {message_id}… ({path})");
                            let job = ProcessCdnLog::new(region, bucket, path.as_ref().to_owned());

                            job.enqueue(&mut conn).with_context(|| {
                                format!("Failed to enqueue processing job for message {message_id}")
                            })?;

                            debug!("Enqueued processing job for message {message_id}");
                        }

                        Ok::<_, anyhow::Error>(())
                    }
                })
                .await?;

                debug!("Processed message: {message_id}");
            }
        }

        Ok(())
    }

    fn is_ignored_path(path: &str) -> bool {
        path.contains("/index.staging.crates.io/") || path.contains("/index.crates.io/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_sqs::operation::receive_message::builders::ReceiveMessageOutputBuilder;
    use aws_sdk_sqs::types::builders::MessageBuilder;
    use aws_sdk_sqs::types::Message;
    use crates_io_test_db::TestDatabase;
    use crates_io_worker::schema::background_jobs;
    use diesel::prelude::*;
    use diesel::r2d2::{ConnectionManager, Pool};
    use diesel::QueryDsl;
    use insta::assert_snapshot;
    use parking_lot::Mutex;

    #[tokio::test]
    async fn test_process_cdn_log() {
        let _guard = crate::util::tracing::init_for_test();

        let path = "cloudfront/index.staging.crates.io/E35K556QRQDZXW.2024-01-16-16.d01d5f13.gz";

        let job = ProcessCdnLog::new(
            "us-west-1".to_string(),
            "bucket".to_string(),
            path.to_string(),
        );

        let config = CdnLogStorageConfig::memory();
        let store = assert_ok!(job.build_store(&config));

        // Add dummy data into the store
        {
            let bytes =
                include_bytes!("../../../crates_io_cdn_logs/test_data/cloudfront/basic.log.gz");

            store.put(&path.into(), bytes[..].into()).await.unwrap();
        }

        assert_ok!(job.run(store).await);
    }

    #[tokio::test]
    async fn test_s3_builder() {
        let path = "cloudfront/index.staging.crates.io/E35K556QRQDZXW.2024-01-16-16.d01d5f13.gz";

        let job = ProcessCdnLog::new(
            "us-west-1".to_string(),
            "bucket".to_string(),
            path.to_string(),
        );

        let access_key = "access_key".into();
        let secret_key = "secret_key".to_string().into();
        let config = CdnLogStorageConfig::s3(access_key, secret_key);
        assert_ok!(job.build_store(&config));
    }

    #[tokio::test]
    async fn test_process_cdn_log_queue() {
        let _guard = crate::util::tracing::init_for_test();

        let mut queue = Box::new(MockSqsQueue::new());
        queue
            .expect_receive_messages()
            .once()
            .returning(|_max_messages| {
                Ok(ReceiveMessageOutputBuilder::default()
                    .messages(message("123", "us-west-1", "bucket", "path"))
                    .build())
            });

        queue
            .expect_receive_messages()
            .once()
            .returning(|_max_messages| Ok(ReceiveMessageOutputBuilder::default().build()));

        let deleted_handles = record_deleted_handles(&mut queue);

        let test_database = TestDatabase::new();
        let connection_pool = build_connection_pool(test_database.url());

        let job = ProcessCdnLogQueue { max_messages: 100 };
        assert_ok!(job.run(queue, &connection_pool).await);

        assert_snapshot!(deleted_handles.lock().join(","), @"123");
        assert_snapshot!(open_jobs(&mut test_database.connect()), @"us-west-1 | bucket | path");
    }

    #[tokio::test]
    async fn test_process_cdn_log_queue_multi_page() {
        let _guard = crate::util::tracing::init_for_test();

        let mut queue = Box::new(MockSqsQueue::new());
        queue
            .expect_receive_messages()
            .once()
            .returning(|_max_messages| {
                Ok(ReceiveMessageOutputBuilder::default()
                    .messages(message("1", "us-west-1", "bucket", "path1"))
                    .messages(message("2", "us-west-1", "bucket", "path2"))
                    .messages(message("3", "us-west-1", "bucket", "path3"))
                    .messages(message("4", "us-west-1", "bucket", "path4"))
                    .messages(message("5", "us-west-1", "bucket", "path5"))
                    .messages(message("6", "us-west-1", "bucket", "path6"))
                    .messages(message("7", "us-west-1", "bucket", "path7"))
                    .messages(message("8", "us-west-1", "bucket", "path8"))
                    .messages(message("9", "us-west-1", "bucket", "path9"))
                    .messages(message("10", "us-west-1", "bucket", "path10"))
                    .build())
            });

        queue
            .expect_receive_messages()
            .once()
            .returning(|_max_messages| {
                Ok(ReceiveMessageOutputBuilder::default()
                    .messages(message("11", "us-west-1", "bucket", "path11"))
                    .build())
            });

        queue
            .expect_receive_messages()
            .once()
            .returning(|_max_messages| Ok(ReceiveMessageOutputBuilder::default().build()));

        let deleted_handles = record_deleted_handles(&mut queue);

        let test_database = TestDatabase::new();
        let connection_pool = build_connection_pool(test_database.url());

        let job = ProcessCdnLogQueue { max_messages: 100 };
        assert_ok!(job.run(queue, &connection_pool).await);

        assert_snapshot!(deleted_handles.lock().join(","), @"1,2,3,4,5,6,7,8,9,10,11");
        assert_snapshot!(open_jobs(&mut test_database.connect()), @r###"
        us-west-1 | bucket | path1
        us-west-1 | bucket | path2
        us-west-1 | bucket | path3
        us-west-1 | bucket | path4
        us-west-1 | bucket | path5
        us-west-1 | bucket | path6
        us-west-1 | bucket | path7
        us-west-1 | bucket | path8
        us-west-1 | bucket | path9
        us-west-1 | bucket | path10
        us-west-1 | bucket | path11
        "###);
    }

    #[tokio::test]
    async fn test_process_cdn_log_queue_parse_error() {
        let _guard = crate::util::tracing::init_for_test();

        let mut queue = Box::new(MockSqsQueue::new());
        queue
            .expect_receive_messages()
            .once()
            .returning(|_max_messages| {
                let message = MessageBuilder::default()
                    .message_id("1")
                    .receipt_handle("1")
                    .body(serde_json::to_string("{}").unwrap())
                    .build();

                Ok(ReceiveMessageOutputBuilder::default()
                    .messages(message)
                    .build())
            });

        queue
            .expect_receive_messages()
            .once()
            .returning(|_max_messages| Ok(ReceiveMessageOutputBuilder::default().build()));

        let deleted_handles = record_deleted_handles(&mut queue);

        let test_database = TestDatabase::new();
        let connection_pool = build_connection_pool(test_database.url());

        let job = ProcessCdnLogQueue { max_messages: 100 };
        assert_ok!(job.run(queue, &connection_pool).await);

        assert_snapshot!(deleted_handles.lock().join(","), @"1");
        assert_snapshot!(open_jobs(&mut test_database.connect()), @"");
    }

    #[test]
    fn test_ignored_path() {
        let is_ignored = ProcessCdnLogQueue::is_ignored_path;

        let valid_paths = vec![
            "cloudfront/static.crates.io/EJED5RT0WA7HA.2024-02-01-10.6a8be093.gz",
            "cloudfront/static.staging.crates.io/E6OCLKYH9FE8V.2024-02-01-10.5da9e90c.gz",
            "fastly-requests/static.crates.io/2024-02-01T09:00:00.000-4AIwSEQyIFDSzdAT1Fqt.log.zst",
            "fastly-requests/static.staging.crates.io/2024-02-01T09:00:00.000-QPF3Ea8eICqLkzaoC_Wt.log.zst"
        ];
        for path in valid_paths {
            assert!(!is_ignored(path));
        }

        let ignored_paths = vec![
            "cloudfront/index.crates.io/EUGCXGQIH3GQ3.2024-02-01-10.2e068fc2.gz",
            "cloudfront/index.staging.crates.io/E35K556QRQDZXW.2024-02-01-10.900ddeaf.gz",
        ];
        for path in ignored_paths {
            assert!(is_ignored(path));
        }
    }

    fn record_deleted_handles(queue: &mut MockSqsQueue) -> Arc<Mutex<Vec<String>>> {
        let deleted_handles = Arc::new(Mutex::new(vec![]));

        queue.expect_delete_message().returning({
            let deleted_handles = deleted_handles.clone();
            move |receipt_handle| {
                deleted_handles.lock().push(receipt_handle.to_owned());
                Ok(())
            }
        });

        deleted_handles
    }

    fn build_connection_pool(url: &str) -> DieselPool {
        let pool = Pool::builder().build(ConnectionManager::new(url)).unwrap();
        DieselPool::new_background_worker(pool)
    }

    fn message(id: &str, region: &str, bucket: &str, path: &str) -> Message {
        let json = json!({
            "Records": [{
                "awsRegion": region,
                "s3": {
                    "bucket": { "name": bucket },
                    "object": { "key": path },
                }
            }]
        });

        MessageBuilder::default()
            .message_id(id)
            .receipt_handle(id)
            .body(serde_json::to_string(&json).unwrap())
            .build()
    }

    fn open_jobs(conn: &mut PgConnection) -> String {
        let jobs = background_jobs::table
            .select((background_jobs::job_type, background_jobs::data))
            .load::<(String, serde_json::Value)>(conn)
            .unwrap();

        jobs.into_iter()
            .inspect(|(job_type, _data)| assert_eq!(job_type, ProcessCdnLog::JOB_NAME))
            .map(|(_job_type, data)| data)
            .map(|data| serde_json::from_value::<ProcessCdnLog>(data).unwrap())
            .map(|job| format!("{} | {} | {}", job.region, job.bucket, job.path))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
