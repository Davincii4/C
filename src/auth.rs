use crate::controllers;
use crate::db::RequestTransaction;
use crate::middleware::log_request;
use crate::models::token::{CrateScope, EndpointScope};
use crate::models::{ApiToken, User};
use crate::util::errors::{
    account_locked, forbidden, internal, AppError, AppResult, InsecurelyGeneratedTokenRevoked,
};
use chrono::Utc;
use conduit::RequestExt;
use conduit_cookie::RequestSession;
use http::header;

#[derive(Debug, Clone)]
pub struct AuthCheck {
    allow_token: bool,
    endpoint_scope: Option<EndpointScope>,
    crate_name: Option<String>,
}

impl AuthCheck {
    #[must_use]
    // #[must_use] can't be applied in the `Default` trait impl
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Self {
        Self {
            allow_token: true,
            endpoint_scope: None,
            crate_name: None,
        }
    }

    #[must_use]
    pub fn only_cookie() -> Self {
        Self {
            allow_token: false,
            endpoint_scope: None,
            crate_name: None,
        }
    }

    pub fn with_endpoint_scope(&self, endpoint_scope: EndpointScope) -> Self {
        Self {
            allow_token: self.allow_token,
            endpoint_scope: Some(endpoint_scope),
            crate_name: self.crate_name.clone(),
        }
    }

    pub fn for_crate(&self, crate_name: &str) -> Self {
        Self {
            allow_token: self.allow_token,
            endpoint_scope: self.endpoint_scope,
            crate_name: Some(crate_name.to_string()),
        }
    }

    pub fn check(&self, request: &dyn RequestExt) -> AppResult<AuthenticatedUser> {
        controllers::util::verify_origin(request)?;

        let auth = authenticate_user(request)?;

        if let Some(reason) = &auth.user.account_lock_reason {
            let still_locked = if let Some(until) = auth.user.account_lock_until {
                until > Utc::now().naive_utc()
            } else {
                true
            };
            if still_locked {
                return Err(account_locked(reason, auth.user.account_lock_until));
            }
        }

        log_request::add_custom_metadata(request, "uid", auth.user_id());
        if let Some(id) = auth.api_token_id() {
            log_request::add_custom_metadata(request, "tokenid", id);
        }

        if let Some(ref token) = auth.token {
            if !self.allow_token {
                let error_message =
                    "API Token authentication was explicitly disallowed for this API";
                return Err(internal(error_message).chain(forbidden()));
            }

            if !self.endpoint_scope_matches(token.endpoint_scopes.as_ref()) {
                let error_message = "Endpoint scope mismatch";
                return Err(internal(error_message).chain(forbidden()));
            }

            if !self.crate_scope_matches(token.crate_scopes.as_ref()) {
                let error_message = "Crate scope mismatch";
                return Err(internal(error_message).chain(forbidden()));
            }
        }

        Ok(auth)
    }

    fn endpoint_scope_matches(&self, token_scopes: Option<&Vec<EndpointScope>>) -> bool {
        match (&token_scopes, &self.endpoint_scope) {
            // The token is a legacy token.
            (None, _) => true,

            // The token is NOT a legacy token, and the endpoint only allows legacy tokens.
            (Some(_), None) => false,

            // The token is NOT a legacy token, and the endpoint allows a certain endpoint scope or a legacy token.
            (Some(token_scopes), Some(endpoint_scope)) => token_scopes.contains(endpoint_scope),
        }
    }

    fn crate_scope_matches(&self, token_scopes: Option<&Vec<CrateScope>>) -> bool {
        match (&token_scopes, &self.crate_name) {
            // The token is a legacy token.
            (None, _) => true,

            // The token does not have any crate scopes.
            (Some(token_scopes), _) if token_scopes.is_empty() => true,

            // The token has crate scopes, but the endpoint does not deal with crates.
            (Some(_), None) => false,

            // The token is NOT a legacy token, and the endpoint allows a certain endpoint scope or a legacy token.
            (Some(token_scopes), Some(crate_name)) => token_scopes
                .iter()
                .any(|token_scope| token_scope.matches(crate_name)),
        }
    }
}

#[derive(Debug)]
pub struct AuthenticatedUser {
    user: User,
    token: Option<ApiToken>,
}

impl AuthenticatedUser {
    pub fn user_id(&self) -> i32 {
        self.user.id
    }

    pub fn api_token_id(&self) -> Option<i32> {
        self.api_token().map(|token| token.id)
    }

    pub fn api_token(&self) -> Option<&ApiToken> {
        self.token.as_ref()
    }

    pub fn user(self) -> User {
        self.user
    }
}

fn authenticate_user(req: &dyn RequestExt) -> AppResult<AuthenticatedUser> {
    let conn = req.db_write()?;

    let session = req.session();
    let user_id_from_session = session.get("user_id").and_then(|s| s.parse::<i32>().ok());

    if let Some(id) = user_id_from_session {
        let user = User::find(&conn, id)
            .map_err(|err| err.chain(internal("user_id from cookie not found in database")))?;

        return Ok(AuthenticatedUser { user, token: None });
    }

    // Otherwise, look for an `Authorization` header on the request
    let maybe_authorization = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());

    if let Some(header_value) = maybe_authorization {
        let token = ApiToken::find_by_api_token(&conn, header_value).map_err(|e| {
            if e.is::<InsecurelyGeneratedTokenRevoked>() {
                e
            } else {
                e.chain(internal("invalid token")).chain(forbidden())
            }
        })?;

        let user = User::find(&conn, token.user_id)
            .map_err(|err| err.chain(internal("user_id from token not found in database")))?;

        return Ok(AuthenticatedUser {
            user,
            token: Some(token),
        });
    }

    // Unable to authenticate the user
    return Err(internal("no cookie session or auth header found").chain(forbidden()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(scope: &str) -> CrateScope {
        CrateScope::try_from(scope).unwrap()
    }

    #[test]
    fn regular_endpoint() {
        let auth_check = AuthCheck::default();

        assert!(auth_check.endpoint_scope_matches(None));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::PublishNew])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::PublishUpdate])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::Yank])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::ChangeOwners])));

        assert!(auth_check.crate_scope_matches(None));
        assert!(!auth_check.crate_scope_matches(Some(&vec![cs("tokio-console")])));
        assert!(!auth_check.crate_scope_matches(Some(&vec![cs("tokio-*")])));
    }

    #[test]
    fn publish_new_endpoint() {
        let auth_check = AuthCheck::default()
            .with_endpoint_scope(EndpointScope::PublishNew)
            .for_crate("tokio-console");

        assert!(auth_check.endpoint_scope_matches(None));
        assert!(auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::PublishNew])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::PublishUpdate])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::Yank])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::ChangeOwners])));

        assert!(auth_check.crate_scope_matches(None));
        assert!(auth_check.crate_scope_matches(Some(&vec![cs("tokio-console")])));
        assert!(auth_check.crate_scope_matches(Some(&vec![cs("tokio-*")])));
        assert!(!auth_check.crate_scope_matches(Some(&vec![cs("anyhow")])));
        assert!(!auth_check.crate_scope_matches(Some(&vec![cs("actix-*")])));
    }

    #[test]
    fn publish_update_endpoint() {
        let auth_check = AuthCheck::default()
            .with_endpoint_scope(EndpointScope::PublishUpdate)
            .for_crate("tokio-console");

        assert!(auth_check.endpoint_scope_matches(None));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::PublishNew])));
        assert!(auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::PublishUpdate])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::Yank])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::ChangeOwners])));

        assert!(auth_check.crate_scope_matches(None));
        assert!(auth_check.crate_scope_matches(Some(&vec![cs("tokio-console")])));
        assert!(auth_check.crate_scope_matches(Some(&vec![cs("tokio-*")])));
        assert!(!auth_check.crate_scope_matches(Some(&vec![cs("anyhow")])));
        assert!(!auth_check.crate_scope_matches(Some(&vec![cs("actix-*")])));
    }

    #[test]
    fn yank_endpoint() {
        let auth_check = AuthCheck::default()
            .with_endpoint_scope(EndpointScope::Yank)
            .for_crate("tokio-console");

        assert!(auth_check.endpoint_scope_matches(None));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::PublishNew])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::PublishUpdate])));
        assert!(auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::Yank])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::ChangeOwners])));

        assert!(auth_check.crate_scope_matches(None));
        assert!(auth_check.crate_scope_matches(Some(&vec![cs("tokio-console")])));
        assert!(auth_check.crate_scope_matches(Some(&vec![cs("tokio-*")])));
        assert!(!auth_check.crate_scope_matches(Some(&vec![cs("anyhow")])));
        assert!(!auth_check.crate_scope_matches(Some(&vec![cs("actix-*")])));
    }

    #[test]
    fn owner_change_endpoint() {
        let auth_check = AuthCheck::default()
            .with_endpoint_scope(EndpointScope::ChangeOwners)
            .for_crate("tokio-console");

        assert!(auth_check.endpoint_scope_matches(None));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::PublishNew])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::PublishUpdate])));
        assert!(!auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::Yank])));
        assert!(auth_check.endpoint_scope_matches(Some(&vec![EndpointScope::ChangeOwners])));

        assert!(auth_check.crate_scope_matches(None));
        assert!(auth_check.crate_scope_matches(Some(&vec![cs("tokio-console")])));
        assert!(auth_check.crate_scope_matches(Some(&vec![cs("tokio-*")])));
        assert!(!auth_check.crate_scope_matches(Some(&vec![cs("anyhow")])));
        assert!(!auth_check.crate_scope_matches(Some(&vec![cs("actix-*")])));
    }
}
