use crate::builders::PublishBuilder;
use crate::util::{RequestHelper, TestApp};
use http::StatusCode;
use insta::assert_json_snapshot;

#[test]
fn good_keywords() {
    let (_, _, _, token) = TestApp::full().with_token();
    let crate_to_publish = PublishBuilder::new("foo_good_key", "1.0.0")
        .keyword("c++")
        .keyword("crates-io_index")
        .keyword("1password");
    let json = token.publish_crate(crate_to_publish).good();
    assert_eq!(json.krate.name, "foo_good_key");
    assert_eq!(json.krate.max_version, "1.0.0");
}

#[test]
fn bad_keywords() {
    let (_, _, _, token) = TestApp::full().with_token();
    let crate_to_publish =
        PublishBuilder::new("foo_bad_key", "1.0.0").keyword("super-long-keyword-name-oh-no");
    let response = token.publish_crate(crate_to_publish);
    assert_eq!(response.status(), StatusCode::OK);
    assert_json_snapshot!(response.into_json());

    let crate_to_publish = PublishBuilder::new("foo_bad_key", "1.0.0").keyword("?@?%");
    let response = token.publish_crate(crate_to_publish);
    assert_eq!(response.status(), StatusCode::OK);
    assert_json_snapshot!(response.into_json());

    let crate_to_publish = PublishBuilder::new("foo_bad_key", "1.0.0").keyword("áccênts");
    let response = token.publish_crate(crate_to_publish);
    assert_eq!(response.status(), StatusCode::OK);
    assert_json_snapshot!(response.into_json());
}

#[test]
fn too_many_keywords() {
    let (app, _, _, token) = TestApp::full().with_token();
    let response = token.publish_crate(
        PublishBuilder::new("foo", "1.0.0")
            .keyword("one")
            .keyword("two")
            .keyword("three")
            .keyword("four")
            .keyword("five")
            .keyword("six"),
    );
    assert_eq!(response.status(), StatusCode::OK);
    assert_json_snapshot!(response.into_json());
    assert!(app.stored_files().is_empty());
}
