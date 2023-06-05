use crate::builders::{CrateBuilder, PublishBuilder};
use crate::util::{RequestHelper, Response, TestApp};
use crate::OkBool;
use http::StatusCode;

pub trait YankRequestHelper {
    /// Yank the specified version of the specified crate and run all pending background jobs
    fn yank(&self, krate_name: &str, version: &str) -> Response<OkBool>;

    /// Unyank the specified version of the specified crate and run all pending background jobs
    fn unyank(&self, krate_name: &str, version: &str) -> Response<OkBool>;
}

impl<T: RequestHelper> YankRequestHelper for T {
    fn yank(&self, krate_name: &str, version: &str) -> Response<OkBool> {
        let url = format!("/api/v1/crates/{krate_name}/{version}/yank");
        let response = self.delete(&url);
        self.app().run_pending_background_jobs();
        response
    }

    fn unyank(&self, krate_name: &str, version: &str) -> Response<OkBool> {
        let url = format!("/api/v1/crates/{krate_name}/{version}/unyank");
        let response = self.put(&url, &[]);
        self.app().run_pending_background_jobs();
        response
    }
}

#[test]
fn yank_by_a_non_owner_fails() {
    let (app, _, _, token) = TestApp::full().with_token();

    let another_user = app.db_new_user("bar");
    let another_user = another_user.as_model();
    app.db(|conn| {
        CrateBuilder::new("foo_not", another_user.id)
            .version("1.0.0")
            .expect_build(conn);
    });

    let response = token.yank("foo_not", "1.0.0");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_json(),
        json!({ "errors": [{ "detail": "must already be an owner to yank or unyank" }] })
    );
}

#[test]
fn yank_records_an_audit_action() {
    let (_, anon, _, token) = TestApp::full().with_token();

    // Upload a new crate, putting it in the git index
    let crate_to_publish = PublishBuilder::new("fyk");
    token.publish_crate(crate_to_publish).good();

    // Yank it
    token.yank("fyk", "1.0.0").good();

    // Make sure it has one publish and one yank audit action
    let json = anon.show_version("fyk", "1.0.0");
    let actions = json.version.audit_actions;

    assert_eq!(actions.len(), 2);
    let action = &actions[1];
    assert_eq!(action.action, "yank");
    assert_eq!(action.user.id, token.as_model().user_id);
}

#[test]
fn unyank_records_an_audit_action() {
    let (_, anon, _, token) = TestApp::full().with_token();

    // Upload a new crate
    let crate_to_publish = PublishBuilder::new("fyk");
    token.publish_crate(crate_to_publish).good();

    // Yank version 1.0.0
    token.yank("fyk", "1.0.0").good();

    // Unyank version 1.0.0
    token.unyank("fyk", "1.0.0").good();

    // Make sure it has one publish, one yank, and one unyank audit action
    let json = anon.show_version("fyk", "1.0.0");
    let actions = json.version.audit_actions;

    assert_eq!(actions.len(), 3);
    let action = &actions[2];
    assert_eq!(action.action, "unyank");
    assert_eq!(action.user.id, token.as_model().user_id);
}

mod auth {
    use super::*;
    use crate::util::{MockAnonymousUser, MockCookieUser};
    use crates_io::models::token::{CrateScope, EndpointScope};

    const CRATE_NAME: &str = "fyk";
    const CRATE_VERSION: &str = "1.0.0";

    fn prepare() -> (TestApp, MockAnonymousUser, MockCookieUser) {
        let (app, anon, cookie) = TestApp::full().with_user();

        let pb = PublishBuilder::new(CRATE_NAME).version(CRATE_VERSION);
        cookie.publish_crate(pb).good();

        (app, anon, cookie)
    }

    #[test]
    fn unauthenticated() {
        let (_, client, _) = prepare();

        let response = client.yank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.into_json(),
            json!({ "errors": [{ "detail": "must be logged in to perform that action" }] })
        );

        let response = client.unyank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.into_json(),
            json!({ "errors": [{ "detail": "must be logged in to perform that action" }] })
        );
    }

    #[test]
    fn cookie_user() {
        let (_, _, client) = prepare();

        let response = client.yank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_json(), json!({ "ok": true }));

        let response = client.unyank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_json(), json!({ "ok": true }));
    }

    #[test]
    fn token_user() {
        let (_, _, client) = prepare();
        let client = client.db_new_token("test-token");

        let response = client.yank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_json(), json!({ "ok": true }));

        let response = client.unyank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_json(), json!({ "ok": true }));
    }

    #[test]
    fn token_user_with_correct_endpoint_scope() {
        let (_, _, client) = prepare();
        let client =
            client.db_new_scoped_token("test-token", None, Some(vec![EndpointScope::Yank]));

        let response = client.yank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_json(), json!({ "ok": true }));

        let response = client.unyank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_json(), json!({ "ok": true }));
    }

    #[test]
    fn token_user_with_incorrect_endpoint_scope() {
        let (_, _, client) = prepare();
        let client = client.db_new_scoped_token(
            "test-token",
            None,
            Some(vec![EndpointScope::PublishUpdate]),
        );

        let response = client.yank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.into_json(),
            json!({ "errors": [{ "detail": "must be logged in to perform that action" }] })
        );

        let response = client.unyank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.into_json(),
            json!({ "errors": [{ "detail": "must be logged in to perform that action" }] })
        );
    }

    #[test]
    fn token_user_with_correct_crate_scope() {
        let (_, _, client) = prepare();
        let client = client.db_new_scoped_token(
            "test-token",
            Some(vec![CrateScope::try_from(CRATE_NAME).unwrap()]),
            None,
        );

        let response = client.yank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_json(), json!({ "ok": true }));

        let response = client.unyank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_json(), json!({ "ok": true }));
    }

    #[test]
    fn token_user_with_correct_wildcard_crate_scope() {
        let (_, _, client) = prepare();
        let wildcard = format!("{}*", CRATE_NAME.chars().next().unwrap());
        let client = client.db_new_scoped_token(
            "test-token",
            Some(vec![CrateScope::try_from(wildcard).unwrap()]),
            None,
        );

        let response = client.yank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_json(), json!({ "ok": true }));

        let response = client.unyank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.into_json(), json!({ "ok": true }));
    }

    #[test]
    fn token_user_with_incorrect_crate_scope() {
        let (_, _, client) = prepare();
        let client = client.db_new_scoped_token(
            "test-token",
            Some(vec![CrateScope::try_from("foo").unwrap()]),
            None,
        );

        let response = client.yank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.into_json(),
            json!({ "errors": [{ "detail": "must be logged in to perform that action" }] })
        );

        let response = client.unyank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.into_json(),
            json!({ "errors": [{ "detail": "must be logged in to perform that action" }] })
        );
    }

    #[test]
    fn token_user_with_incorrect_wildcard_crate_scope() {
        let (_, _, client) = prepare();
        let client = client.db_new_scoped_token(
            "test-token",
            Some(vec![CrateScope::try_from("foo*").unwrap()]),
            None,
        );

        let response = client.yank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.into_json(),
            json!({ "errors": [{ "detail": "must be logged in to perform that action" }] })
        );

        let response = client.unyank(CRATE_NAME, CRATE_VERSION);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.into_json(),
            json!({ "errors": [{ "detail": "must be logged in to perform that action" }] })
        );
    }
}
