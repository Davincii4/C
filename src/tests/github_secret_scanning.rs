use crate::{RequestHelper, TestApp};
use cargo_registry::controllers::github::secret_scanning::GitHubSecretAlertFeedback;
use cargo_registry::{models::ApiToken, schema::api_tokens};
use conduit::StatusCode;
use diesel::prelude::*;

static URL: &str = "/api/github/secret-scanning/verify";

// Test request and signature from https://docs.github.com/en/developers/overview/secret-scanning-partner-program#create-a-secret-alert-service
static GITHUB_ALERT: &[u8] =
    br#"[{"token":"some_token","type":"some_type","url":"some_url","source":"some_source"}]"#;
static GITHUB_PUBLIC_KEY_IDENTIFIER: &str =
    "f9525bf080f75b3506ca1ead061add62b8633a346606dc5fe544e29231c6ee0d";
static GITHUB_PUBLIC_KEY_SIGNATURE: &str = "MEUCIFLZzeK++IhS+y276SRk2Pe5LfDrfvTXu6iwKKcFGCrvAiEAhHN2kDOhy2I6eGkOFmxNkOJ+L2y8oQ9A2T9GGJo6WJY=";

#[test]
fn github_secret_alert_revokes_token() {
    let (app, anon, user, token) = TestApp::init().with_token();

    // Ensure no emails were sent up to this point
    assert_eq!(0, app.as_inner().emails.mails_in_memory().unwrap().len());

    // Ensure that the token currently exists in the database
    app.db(|conn| {
        let tokens: Vec<ApiToken> = assert_ok!(ApiToken::belonging_to(user.as_model())
            .filter(api_tokens::revoked.eq(false))
            .load(conn));
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].name, token.as_model().name);
    });

    // Set token to expected value in signed request
    app.db(|conn| {
        diesel::update(api_tokens::table)
            .set(api_tokens::token.eq(b"some_token" as &[u8]))
            .execute(conn)
            .unwrap();
    });

    let mut request = anon.post_request(URL);
    request.with_body(GITHUB_ALERT);
    request.header("GITHUB-PUBLIC-KEY-IDENTIFIER", GITHUB_PUBLIC_KEY_IDENTIFIER);
    request.header("GITHUB-PUBLIC-KEY-SIGNATURE", GITHUB_PUBLIC_KEY_SIGNATURE);
    let response = anon.run::<Vec<GitHubSecretAlertFeedback>>(request);
    assert_eq!(response.status(), StatusCode::OK);

    // Ensure feedback is a true positive
    let feedback = response.good();
    assert_eq!(feedback.len(), 1);
    assert_eq!(feedback[0].token_raw, "some_token");
    assert_eq!(feedback[0].token_type, "some_type");
    assert_eq!(feedback[0].label, "true_positive");

    // Ensure that the token was revoked
    app.db(|conn| {
        let tokens: Vec<ApiToken> = assert_ok!(ApiToken::belonging_to(user.as_model())
            .filter(api_tokens::revoked.eq(false))
            .load(conn));
        assert_eq!(tokens.len(), 0);
        let tokens: Vec<ApiToken> = assert_ok!(ApiToken::belonging_to(user.as_model())
            .filter(api_tokens::revoked.eq(true))
            .load(conn));
        assert_eq!(tokens.len(), 1);
    });

    // Ensure exactly one email was sent
    assert_eq!(1, app.as_inner().emails.mails_in_memory().unwrap().len());
}

#[test]
fn github_secret_alert_for_unknown_token() {
    let (app, anon, user, token) = TestApp::init().with_token();

    // Ensure no emails were sent up to this point
    assert_eq!(0, app.as_inner().emails.mails_in_memory().unwrap().len());

    // Ensure that the token currently exists in the database
    app.db(|conn| {
        let tokens: Vec<ApiToken> = assert_ok!(ApiToken::belonging_to(user.as_model())
            .filter(api_tokens::revoked.eq(false))
            .load(conn));
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].name, token.as_model().name);
    });

    let mut request = anon.post_request(URL);
    request.with_body(GITHUB_ALERT);
    request.header("GITHUB-PUBLIC-KEY-IDENTIFIER", GITHUB_PUBLIC_KEY_IDENTIFIER);
    request.header("GITHUB-PUBLIC-KEY-SIGNATURE", GITHUB_PUBLIC_KEY_SIGNATURE);
    let response = anon.run::<Vec<GitHubSecretAlertFeedback>>(request);
    assert_eq!(response.status(), StatusCode::OK);

    // Ensure feedback is a false positive
    let feedback = response.good();
    assert_eq!(feedback.len(), 1);
    assert_eq!(feedback[0].token_raw, "some_token");
    assert_eq!(feedback[0].token_type, "some_type");
    assert_eq!(feedback[0].label, "false_positive");

    // Ensure that the token was not revoked
    app.db(|conn| {
        let tokens: Vec<ApiToken> = assert_ok!(ApiToken::belonging_to(user.as_model())
            .filter(api_tokens::revoked.eq(false))
            .load(conn));
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].name, token.as_model().name);
    });

    // Ensure still no emails were sent
    assert_eq!(0, app.as_inner().emails.mails_in_memory().unwrap().len());
}

#[test]
fn github_secret_alert_invalid_signature_fails() {
    let (_, anon) = TestApp::init().empty();

    // No headers or request body
    let request = anon.post_request(URL);
    let response = anon.run::<()>(request);
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Request body but no headers
    let mut request = anon.post_request(URL);
    request.with_body(GITHUB_ALERT);
    let response = anon.run::<()>(request);
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Headers but no request body
    let mut request = anon.post_request(URL);
    request.header("GITHUB-PUBLIC-KEY-IDENTIFIER", GITHUB_PUBLIC_KEY_IDENTIFIER);
    request.header("GITHUB-PUBLIC-KEY-SIGNATURE", GITHUB_PUBLIC_KEY_SIGNATURE);
    let response = anon.run::<()>(request);
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Request body but only key identifier header
    let mut request = anon.post_request(URL);
    request.with_body(GITHUB_ALERT);
    request.header("GITHUB-PUBLIC-KEY-IDENTIFIER", GITHUB_PUBLIC_KEY_IDENTIFIER);
    let response = anon.run::<()>(request);
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Invalid signature
    let mut request = anon.post_request(URL);
    request.with_body(GITHUB_ALERT);
    request.header("GITHUB-PUBLIC-KEY-IDENTIFIER", GITHUB_PUBLIC_KEY_IDENTIFIER);
    request.header("GITHUB-PUBLIC-KEY-SIGNATURE", "bad signature");
    let response = anon.run::<()>(request);
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Invalid signature that is valid base64
    let mut request = anon.post_request(URL);
    request.with_body(GITHUB_ALERT);
    request.header("GITHUB-PUBLIC-KEY-IDENTIFIER", GITHUB_PUBLIC_KEY_IDENTIFIER);
    request.header("GITHUB-PUBLIC-KEY-SIGNATURE", "YmFkIHNpZ25hdHVyZQ==");
    let response = anon.run::<()>(request);
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
