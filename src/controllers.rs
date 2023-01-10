mod cargo_prelude {
    pub use super::prelude::*;
    pub use crate::util::errors::cargo_err;
}

mod frontend_prelude {
    pub use super::prelude::*;
    pub use crate::util::errors::{bad_request, server_error};
}

mod prelude {
    pub use super::helpers::ok_true;
    pub use axum::extract::{Path, State};
    pub use axum::response::{IntoResponse, Response};
    pub use axum::Json;
    pub use diesel::prelude::*;
    pub use serde_json::Value;

    pub use conduit_axum::ConduitRequest;
    pub use http::{header, request::Parts, Request, StatusCode};

    pub use super::conduit_axum::conduit_compat;
    pub use crate::app::AppState;
    use crate::controllers::util::RequestPartsExt;
    pub use crate::middleware::app::RequestApp;
    pub use crate::util::errors::{cargo_err, AppError, AppResult, BoxedAppError};
    use indexmap::IndexMap;

    pub trait RequestUtils {
        fn redirect(&self, url: String) -> Response {
            (StatusCode::FOUND, [(header::LOCATION, url)]).into_response()
        }

        fn query(&self) -> IndexMap<String, String>;
        fn wants_json(&self) -> bool;
        fn query_with_params(&self, params: IndexMap<String, String>) -> String;
    }

    impl<T: RequestPartsExt> RequestUtils for T {
        fn query(&self) -> IndexMap<String, String> {
            url::form_urlencoded::parse(self.uri().query().unwrap_or("").as_bytes())
                .into_owned()
                .collect()
        }

        fn wants_json(&self) -> bool {
            self.headers()
                .get_all(header::ACCEPT)
                .iter()
                .any(|val| val.to_str().unwrap_or_default().contains("json"))
        }

        fn query_with_params(&self, new_params: IndexMap<String, String>) -> String {
            let mut params = self.query();
            params.extend(new_params);
            let query_string = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(params)
                .finish();
            format!("?{query_string}")
        }
    }
}

pub mod helpers;
pub mod util;

pub mod category;
mod conduit_axum;
pub mod crate_owner_invitation;
pub mod git;
pub mod github;
pub mod keyword;
pub mod krate;
pub mod metrics;
pub mod site_metadata;
pub mod team;
pub mod token;
pub mod user;
pub mod version;
