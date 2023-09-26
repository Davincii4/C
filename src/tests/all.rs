#![warn(clippy::all, rust_2018_idioms)]

#[macro_use]
extern crate claims;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate serde_json;

use crate::util::{RequestHelper, TestApp};
use crates_io::{
    models::{Crate, CrateOwner, NewCategory, NewTeam, NewUser, Team, User},
    schema::crate_owners,
    views::{
        EncodableCategory, EncodableCategoryWithSubcategories, EncodableCrate, EncodableKeyword,
        EncodableOwner, EncodableVersion, GoodCrate,
    },
};
use std::{
    borrow::Cow,
    sync::atomic::{AtomicUsize, Ordering},
};

use diesel::prelude::*;

mod account_lock;
mod authentication;
mod blocked_routes;
mod builders;
mod categories;
mod dump_db;
mod github_secret_scanning;
mod krate;
mod middleware;
mod models;
mod not_found_error;
mod owners;
mod pagination;
mod read_only_mode;
mod routes;
mod schema_details;
mod server;
mod server_binary;
mod team;
mod token;
mod unhealthy_database;
mod user;
mod util;
mod version;
mod worker;

#[derive(Deserialize)]
pub struct CrateList {
    crates: Vec<EncodableCrate>,
    meta: CrateMeta,
}
#[derive(Deserialize)]
struct CrateMeta {
    total: i32,
    next_page: Option<String>,
    prev_page: Option<String>,
}
#[derive(Deserialize)]
pub struct CrateResponse {
    #[serde(rename = "crate")]
    krate: EncodableCrate,
    versions: Option<Vec<EncodableVersion>>,
    keywords: Option<Vec<EncodableKeyword>>,
}
#[derive(Deserialize)]
pub struct VersionResponse {
    version: EncodableVersion,
}
#[derive(Deserialize)]
pub struct OwnerTeamsResponse {
    teams: Vec<EncodableOwner>,
}
#[derive(Deserialize)]
pub struct OwnersResponse {
    users: Vec<EncodableOwner>,
}
#[derive(Deserialize)]
pub struct CategoryResponse {
    category: EncodableCategoryWithSubcategories,
}
#[derive(Deserialize)]
pub struct CategoryListResponse {
    categories: Vec<EncodableCategory>,
    meta: CategoryMeta,
}
#[derive(Deserialize)]
pub struct CategoryMeta {
    total: i32,
}
#[derive(Deserialize)]
pub struct OkBool {
    ok: bool,
}

// Return the environment variable only if it has been defined
#[track_caller]
fn env(var: &str) -> String {
    match dotenvy::var(var) {
        Ok(ref s) if s.is_empty() => panic!("environment variable `{var}` must not be empty"),
        Ok(s) => s,
        _ => panic!("environment variable `{var}` must be defined and valid unicode"),
    }
}

static NEXT_GH_ID: AtomicUsize = AtomicUsize::new(0);

fn new_user(login: &str) -> NewUser<'_> {
    NewUser {
        gh_id: NEXT_GH_ID.fetch_add(1, Ordering::SeqCst) as i32,
        gh_login: login,
        name: None,
        gh_avatar: None,
        gh_access_token: Cow::Borrowed("some random token"),
    }
}

fn new_team(login: &str) -> NewTeam<'_> {
    NewTeam {
        org_id: NEXT_GH_ID.fetch_add(1, Ordering::SeqCst) as i32,
        github_id: NEXT_GH_ID.fetch_add(1, Ordering::SeqCst) as i32,
        login,
        name: None,
        avatar: None,
    }
}

fn add_team_to_crate(
    t: &Team,
    krate: &Crate,
    u: &User,
    conn: &mut PgConnection,
) -> QueryResult<()> {
    let crate_owner = CrateOwner {
        crate_id: krate.id,
        owner_id: t.id,
        created_by: u.id,
        owner_kind: 1, // Team owner kind is 1 according to owner.rs
        email_notifications: true,
    };

    diesel::insert_into(crate_owners::table)
        .values(&crate_owner)
        .on_conflict(crate_owners::table.primary_key())
        .do_update()
        .set(crate_owners::deleted.eq(false))
        .execute(conn)?;

    Ok(())
}

fn new_category<'a>(category: &'a str, slug: &'a str, description: &'a str) -> NewCategory<'a> {
    NewCategory {
        category,
        slug,
        description,
    }
}

// This reflects the configuration of our test environment. In the production application, this
// does not hold true.
#[test]
#[should_panic]
fn recursive_get_of_db_conn_in_tests_will_panic() {
    let (app, _) = TestApp::init().empty();
    let app = app.as_inner();

    let _conn1 = app.db_write().unwrap();
    let _conn2 = app.db_write().unwrap();
}
