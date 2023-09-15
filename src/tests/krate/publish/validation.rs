use crate::builders::PublishBuilder;
use crate::util::{RequestHelper, TestApp};
use crates_io::models::krate::MAX_NAME_LENGTH;
use http::StatusCode;
use insta::assert_json_snapshot;

#[test]
fn invalid_names() {
    let (app, _, _, token) = TestApp::full().with_token();

    let bad_name = |name: &str| {
        let crate_to_publish = PublishBuilder::new(name, "1.0.0");
        let response = token.publish_crate(crate_to_publish);
        assert_eq!(response.status(), StatusCode::OK);
        assert_json_snapshot!(response.into_json());
    };

    bad_name("");
    bad_name("foo bar");
    bad_name(&"a".repeat(MAX_NAME_LENGTH + 1));
    bad_name("snow☃");
    bad_name("áccênts");

    bad_name("std");
    bad_name("STD");
    bad_name("compiler-rt");
    bad_name("compiler_rt");
    bad_name("coMpiLer_Rt");

    assert!(app.stored_files().is_empty());
}

#[test]
fn license_and_description_required() {
    let (app, _, _, token) = TestApp::full().with_token();

    let crate_to_publish = PublishBuilder::new("foo_metadata", "1.1.0")
        .unset_license()
        .unset_description();

    let response = token.publish_crate(crate_to_publish);
    assert_eq!(response.status(), StatusCode::OK);
    assert_json_snapshot!(response.into_json());

    let crate_to_publish = PublishBuilder::new("foo_metadata", "1.1.0").unset_description();

    let response = token.publish_crate(crate_to_publish);
    assert_eq!(response.status(), StatusCode::OK);
    assert_json_snapshot!(response.into_json());

    let crate_to_publish = PublishBuilder::new("foo_metadata", "1.1.0")
        .unset_license()
        .license_file("foo")
        .unset_description();

    let response = token.publish_crate(crate_to_publish);
    assert_eq!(response.status(), StatusCode::OK);
    assert_json_snapshot!(response.into_json());

    assert!(app.stored_files().is_empty());
}
