use super::{MockAnonymousUser, MockCookieUser, MockTokenUser};
use crate::record;
use crate::util::{chaosproxy::ChaosProxy, fresh_schema::FreshSchema};
use cargo_registry::config::{self, BalanceCapacityConfig, DbPoolConfig};
use cargo_registry::{background_jobs::Environment, App, Emails};
use cargo_registry_index::testing::UpstreamIndex;
use cargo_registry_index::{Credentials, Repository as WorkerRepository, RepositoryConfig};
use std::{rc::Rc, sync::Arc, time::Duration};

use crate::util::github::{MockGitHubClient, MOCK_GITHUB_DATA};
use cargo_registry::models::token::{CrateScope, EndpointScope};
use cargo_registry::swirl::Runner;
use diesel::PgConnection;
use oauth2::{ClientId, ClientSecret};
use reqwest::{blocking::Client, Proxy};
use std::collections::HashSet;

struct TestAppInner {
    app: Arc<App>,
    // The bomb (if created) needs to be held in scope until the end of the test.
    _bomb: Option<record::Bomb>,
    router: axum::Router,
    index: Option<UpstreamIndex>,
    runner: Option<Runner>,

    primary_db_chaosproxy: Option<Arc<ChaosProxy>>,
    replica_db_chaosproxy: Option<Arc<ChaosProxy>>,

    // Must be the last field of the struct!
    _fresh_schema: Option<FreshSchema>,
}

impl Drop for TestAppInner {
    fn drop(&mut self) {
        use cargo_registry::schema::background_jobs::dsl::*;
        use diesel::prelude::*;

        // Avoid a double-panic if the test is already failing
        if std::thread::panicking() {
            return;
        }

        // Lazily run any remaining jobs
        if let Some(runner) = &self.runner {
            runner.run_all_pending_jobs().expect("Could not run jobs");
            runner.check_for_failed_jobs().expect("Failed jobs remain");
        }

        // Manually verify that all jobs have completed successfully
        // This will catch any tests that enqueued a job but forgot to initialize the runner
        let conn = self.app.primary_database.get().unwrap();
        let job_count: i64 = background_jobs.count().get_result(&*conn).unwrap();
        assert_eq!(
            0, job_count,
            "Unprocessed or failed jobs remain in the queue"
        );

        // TODO: If a runner was started, obtain the clone from it and ensure its HEAD matches the upstream index HEAD
    }
}

/// A representation of the app and its database transaction
#[derive(Clone)]
pub struct TestApp(Rc<TestAppInner>);

impl TestApp {
    /// Initialize an application with an `Uploader` that panics
    pub fn init() -> TestAppBuilder {
        cargo_registry::util::tracing::init_for_test();

        TestAppBuilder {
            config: simple_config(),
            proxy: None,
            bomb: None,
            index: None,
            build_job_runner: false,
            test_database: TestDatabase::TestPool,
        }
    }

    /// Initialize the app and a proxy that can record and playback outgoing HTTP requests
    pub fn with_proxy() -> TestAppBuilder {
        Self::init().with_proxy()
    }

    /// Initialize a full application, with a proxy, index, and background worker
    pub fn full() -> TestAppBuilder {
        Self::with_proxy().with_git_index().with_job_runner()
    }

    /// Obtain the database connection and pass it to the closure
    ///
    /// Within each test, the connection pool only has 1 connection so it is necessary to drop the
    /// connection before making any API calls.  Once the closure returns, the connection is
    /// dropped, ensuring it is returned to the pool and available for any future API calls.
    pub fn db<T, F: FnOnce(&PgConnection) -> T>(&self, f: F) -> T {
        let conn = self.0.app.primary_database.get().unwrap();
        f(&conn)
    }

    /// Create a new user with a verified email address in the database and return a mock user
    /// session
    ///
    /// This method updates the database directly
    pub fn db_new_user(&self, username: &str) -> MockCookieUser {
        use cargo_registry::schema::emails;
        use diesel::prelude::*;

        let user = self.db(|conn| {
            let email = "something@example.com";

            let user = crate::new_user(username)
                .create_or_update(None, &self.0.app.emails, conn)
                .unwrap();
            diesel::insert_into(emails::table)
                .values((
                    emails::user_id.eq(user.id),
                    emails::email.eq(email),
                    emails::verified.eq(true),
                ))
                .execute(conn)
                .unwrap();
            user
        });
        MockCookieUser {
            app: self.clone(),
            user,
        }
    }

    /// Obtain a reference to the upstream repository ("the index")
    pub fn upstream_index(&self) -> &UpstreamIndex {
        assert_some!(self.0.index.as_ref())
    }

    /// Obtain a list of crates from the index HEAD
    pub fn crates_from_index_head(&self, crate_name: &str) -> Vec<cargo_registry_index::Crate> {
        self.upstream_index()
            .crates_from_index_head(crate_name)
            .unwrap()
    }

    #[track_caller]
    pub fn run_pending_background_jobs(&self) {
        let runner = &self.0.runner;
        let runner = runner.as_ref().expect("Index has not been initialized");

        runner.run_all_pending_jobs().expect("Could not run jobs");
        runner
            .check_for_failed_jobs()
            .expect("Could not determine if jobs failed");
    }

    /// Obtain a reference to the inner `App` value
    pub fn as_inner(&self) -> &App {
        &self.0.app
    }

    /// Obtain a reference to the axum Router
    pub fn router(&self) -> &axum::Router {
        &self.0.router
    }

    pub(crate) fn primary_db_chaosproxy(&self) -> Arc<ChaosProxy> {
        self.0
            .primary_db_chaosproxy
            .clone()
            .expect("ChaosProxy is not enabled on this test, call with_database during app init")
    }

    pub(crate) fn replica_db_chaosproxy(&self) -> Arc<ChaosProxy> {
        self.0
            .replica_db_chaosproxy
            .clone()
            .expect("ChaosProxy is not enabled on this test, call with_database during app init")
    }
}

/// Defines the type of test database.
pub enum TestDatabase {
    /// Use the fast test database pool
    TestPool,
    /// Use the slow test database pool with a fresh schema that enables ChaosProxy
    /// TODO rewrite comment, uses a database pool
    SlowRealPool { replica: bool },
}

pub struct TestAppBuilder {
    config: config::Server,
    proxy: Option<String>,
    bomb: Option<record::Bomb>,
    index: Option<UpstreamIndex>,
    build_job_runner: bool,
    test_database: TestDatabase,
}

impl TestAppBuilder {
    /// Create a `TestApp` with an empty database
    pub fn empty(mut self) -> (TestApp, MockAnonymousUser) {
        // Run each test inside a fresh database schema, deleted at the end of the test,
        // The schema will be cleared up once the app is dropped.
        let (primary_db_chaosproxy, replica_db_chaosproxy, fresh_schema) =
            if !self.config.use_test_database_pool {
                let fresh_schema = FreshSchema::new(&self.config.db.primary.url);
                let (primary_proxy, url) =
                    ChaosProxy::proxy_database_url(fresh_schema.database_url()).unwrap();
                self.config.db.primary.url = url;

                let replica_proxy = match self.test_database {
                    TestDatabase::SlowRealPool { replica: true } => {
                        let (replica_proxy, url) =
                            ChaosProxy::proxy_database_url(fresh_schema.database_url()).unwrap();
                        self.config.db.replica = Some(DbPoolConfig {
                            url,
                            read_only_mode: true,
                            pool_size: 1,
                            min_idle: None,
                        });
                        Some(replica_proxy)
                    }
                    _ => None,
                };

                (Some(primary_proxy), replica_proxy, Some(fresh_schema))
            } else {
                (None, None, None)
            };

        let (app, router) = build_app(self.config, self.proxy);

        let runner = if self.build_job_runner {
            let repository_config = RepositoryConfig {
                index_location: UpstreamIndex::url(),
                credentials: Credentials::Missing,
            };
            let index = WorkerRepository::open(&repository_config).expect("Could not clone index");
            let environment = Environment::new(
                index,
                app.config.uploader().clone(),
                app.http_client().clone(),
                None,
            );

            Some(Runner::test_runner(
                environment,
                app.primary_database.clone(),
            ))
        } else {
            None
        };

        let test_app_inner = TestAppInner {
            app,
            _fresh_schema: fresh_schema,
            _bomb: self.bomb,
            router,
            index: self.index,
            runner,
            primary_db_chaosproxy,
            replica_db_chaosproxy,
        };
        let test_app = TestApp(Rc::new(test_app_inner));
        let anon = MockAnonymousUser {
            app: test_app.clone(),
        };
        (test_app, anon)
    }

    /// Create a proxy for use with this app
    pub fn with_proxy(mut self) -> Self {
        let (proxy, bomb) = record::proxy();
        self.proxy = Some(proxy);
        self.bomb = Some(bomb);
        self
    }

    // Create a `TestApp` with a database including a default user
    pub fn with_user(self) -> (TestApp, MockAnonymousUser, MockCookieUser) {
        let (app, anon) = self.empty();
        let user = app.db_new_user("foo");
        (app, anon, user)
    }

    /// Create a `TestApp` with a database including a default user and its token
    pub fn with_token(self) -> (TestApp, MockAnonymousUser, MockCookieUser, MockTokenUser) {
        let (app, anon) = self.empty();
        let user = app.db_new_user("foo");
        let token = user.db_new_token("bar");
        (app, anon, user, token)
    }

    pub fn with_scoped_token(
        self,
        crate_scopes: Option<Vec<CrateScope>>,
        endpoint_scopes: Option<Vec<EndpointScope>>,
    ) -> (TestApp, MockAnonymousUser, MockCookieUser, MockTokenUser) {
        let (app, anon) = self.empty();
        let user = app.db_new_user("foo");
        let token = user.db_new_scoped_token("bar", crate_scopes, endpoint_scopes);
        (app, anon, user, token)
    }

    pub fn with_config(mut self, f: impl FnOnce(&mut config::Server)) -> Self {
        f(&mut self.config);
        self
    }

    pub fn with_publish_rate_limit(self, rate: Duration, burst: i32) -> Self {
        self.with_config(|config| {
            config.publish_rate_limit.rate = rate;
            config.publish_rate_limit.burst = burst;
        })
    }

    pub fn with_git_index(mut self) -> Self {
        self.index = Some(UpstreamIndex::new().unwrap());
        self
    }

    pub fn with_job_runner(mut self) -> Self {
        self.build_job_runner = true;
        self
    }

    /// Configures the test database
    pub fn with_database(mut self, test_database: TestDatabase) -> Self {
        self.config.use_test_database_pool = false;
        self.test_database = test_database;
        self
    }
}

fn simple_config() -> config::Server {
    config::Server {
        base: config::Base::test(),
        db: config::DatabasePools::test_from_environment(),
        session_key: cookie::Key::derive_from("test this has to be over 32 bytes long".as_bytes()),
        gh_client_id: ClientId::new(dotenv::var("GH_CLIENT_ID").unwrap_or_default()),
        gh_client_secret: ClientSecret::new(dotenv::var("GH_CLIENT_SECRET").unwrap_or_default()),
        max_upload_size: 3000,
        max_unpack_size: 2000,
        publish_rate_limit: Default::default(),
        new_version_rate_limit: Some(10),
        blocked_traffic: Default::default(),
        max_allowed_page_offset: 200,
        page_offset_ua_blocklist: vec![],
        page_offset_cidr_blocklist: vec![],
        excluded_crate_names: vec![],
        domain_name: "crates.io".into(),
        allowed_origins: Default::default(),
        downloads_persist_interval_ms: 1000,
        ownership_invitations_expiration_days: 30,
        metrics_authorization_token: None,
        use_test_database_pool: true,
        instance_metrics_log_every_seconds: None,
        force_unconditional_redirects: false,
        blocked_routes: HashSet::new(),
        version_id_cache_size: 10000,
        version_id_cache_ttl: Duration::from_secs(5 * 60),
        cdn_user_agent: "Amazon CloudFront".to_string(),
        balance_capacity: BalanceCapacityConfig::for_testing(),
    }
}

fn build_app(config: config::Server, proxy: Option<String>) -> (Arc<App>, axum::Router) {
    let client = if let Some(proxy) = proxy {
        let mut builder = Client::builder();
        builder = builder
            .proxy(Proxy::all(proxy).expect("Unable to configure proxy with the provided URL"));
        Some(builder.build().expect("TLS backend cannot be initialized"))
    } else {
        None
    };

    let mut app = App::new(config, client);

    // Use the in-memory email backend for all tests, allowing tests to analyze the emails sent by
    // the application. This will also prevent cluttering the filesystem.
    app.emails = Emails::new_in_memory();

    // Use a custom mock for the GitHub client, allowing to define the GitHub users and
    // organizations without actually having to create GitHub accounts.
    app.github = Box::new(MockGitHubClient::new(&MOCK_GITHUB_DATA));

    let app = Arc::new(app);
    let router = cargo_registry::build_handler(Arc::clone(&app));
    (app, router)
}
