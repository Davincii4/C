//! Runs enqueued background jobs
//!
//! This binary will loop until interrupted. It will run all jobs in the
//! background queue, sleeping for 1 second whenever the queue is empty. If we
//! are unable to spawn workers to run jobs (either because we couldn't connect
//! to the DB, an error occurred while loading, or we just never heard back from
//! the worker thread), we will rebuild the runner and try again up to 5 times.
//! After the 5th occurrence, we will panic.
//!
//! Usage:
//!      cargo run --bin background-worker

#![warn(clippy::all, rust_2018_idioms)]

#[macro_use]
extern crate tracing;

use cargo_registry::config;
use cargo_registry::worker::cloudfront::CloudFront;
use cargo_registry::{background_jobs::*, db};
use cargo_registry_index::{Repository, RepositoryConfig};
use reqwest::blocking::Client;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use cargo_registry::swirl;

fn main() {
    let _sentry = cargo_registry::sentry::init();

    // Initialize logging
    cargo_registry::util::tracing::init();

    info!("Booting runner");

    let config = config::Server::default();
    let uploader = config.base.uploader();

    if config.db.are_all_read_only() {
        loop {
            warn!(
                "Cannot run background jobs with a read-only pool. Please scale background_worker \
                to 0 processes until the leader database is available."
            );
            sleep(Duration::from_secs(60));
        }
    }

    let db_url = db::connection_url(&config.db, &config.db.primary.url);

    let job_start_timeout = dotenv::var("BACKGROUND_JOB_TIMEOUT")
        .unwrap_or_else(|_| "30".into())
        .parse()
        .expect("Invalid value for `BACKGROUND_JOB_TIMEOUT`");

    info!("Cloning index");

    let repository_config = RepositoryConfig::from_environment();
    let repository = Arc::new(Mutex::new(
        Repository::open(&repository_config).expect("Failed to clone index"),
    ));
    info!("Index cloned");

    let cloudfront = CloudFront::from_environment();

    let build_runner = || {
        let client = Client::builder()
            .timeout(Duration::from_secs(45))
            .build()
            .expect("Couldn't build client");
        let environment = Environment::new_shared(
            repository.clone(),
            uploader.clone(),
            client,
            cloudfront.clone(),
        );
        swirl::Runner::production_runner(environment, db_url.clone(), job_start_timeout)
    };
    let mut runner = build_runner();

    info!("Runner booted, running jobs");

    let mut failure_count = 0;

    loop {
        if let Err(e) = runner.run_all_pending_jobs() {
            failure_count += 1;
            if failure_count < 5 {
                warn!(?failure_count, err = ?e, "Error running jobs -- retrying");
                runner = build_runner();
            } else {
                panic!("Failed to begin running jobs 5 times. Restarting the process");
            }
        }
        sleep(Duration::from_secs(1));
    }
}
