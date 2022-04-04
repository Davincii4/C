use crate::controllers::frontend_prelude::*;

use conduit_cookie::{RequestCookies, RequestSession};
use cookie::{Cookie, SameSite};
use oauth2::reqwest::http_client;
use oauth2::{AuthorizationCode, Scope, TokenResponse};

use crate::email::Emails;
use crate::github::GithubUser;
use crate::models::{NewUser, PersistentSession, User};
use crate::schema::users;
use crate::util::errors::ReadOnlyMode;
use crate::util::token::NewSecureToken;
use crate::util::token::SecureToken;
use crate::Env;

pub const SESSION_COOKIE_NAME: &str = "crates_auth";

pub fn session_cookie(token: &NewSecureToken, secure: bool) -> Cookie<'static> {
    Cookie::build(SESSION_COOKIE_NAME, token.plaintext().to_string())
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Strict)
        .path("/")
        .finish()
}

/// Handles the `GET /api/private/session/begin` route.
///
/// This route will return an authorization URL for the GitHub OAuth flow including the crates.io
/// `client_id` and a randomly generated `state` secret.
///
/// see <https://developer.github.com/v3/oauth/#redirect-users-to-request-github-access>
///
/// ## Response Body Example
///
/// ```json
/// {
///     "state": "b84a63c4ea3fcb4ac84",
///     "url": "https://github.com/login/oauth/authorize?client_id=...&state=...&scope=read%3Aorg"
/// }
/// ```
pub fn begin(req: &mut dyn RequestExt) -> EndpointResult {
    let (url, state) = req
        .app()
        .github_oauth
        .authorize_url(oauth2::CsrfToken::new_random)
        .add_scope(Scope::new("read:org".to_string()))
        .url();
    let state = state.secret().to_string();
    req.session_mut()
        .insert("github_oauth_state".to_string(), state.clone());

    Ok(req.json(&json!({ "url": url.to_string(), "state": state })))
}

/// Handles the `GET /api/private/session/authorize` route.
///
/// This route is called from the GitHub API OAuth flow after the user accepted or rejected
/// the data access permissions. It will check the `state` parameter and then call the GitHub API
/// to exchange the temporary `code` for an API token. The API token is returned together with
/// the corresponding user information.
///
/// see <https://developer.github.com/v3/oauth/#github-redirects-back-to-your-site>
///
/// ## Query Parameters
///
/// - `code` – temporary code received from the GitHub API  **(Required)**
/// - `state` – state parameter received from the GitHub API  **(Required)**
///
/// ## Response Body Example
///
/// ```json
/// {
///     "api_token": "b84a63c4ea3fcb4ac84",
///     "user": {
///         "email": "foo@bar.org",
///         "name": "Foo Bar",
///         "login": "foobar",
///         "avatar": "https://avatars.githubusercontent.com/u/1234",
///         "url": null
///     }
/// }
/// ```
pub fn authorize(req: &mut dyn RequestExt) -> EndpointResult {
    // Parse the url query
    let mut query = req.query();
    let code = query.remove("code").unwrap_or_default();
    let state = query.remove("state").unwrap_or_default();

    // Make sure that the state we just got matches the session state that we
    // should have issued earlier.
    {
        let session_state = req.session_mut().remove(&"github_oauth_state".to_string());
        let session_state = session_state.as_deref();
        if Some(&state[..]) != session_state {
            return Err(bad_request("invalid state parameter"));
        }
    }

    // Fetch the access token from GitHub using the code we just got
    let code = AuthorizationCode::new(code);
    let token = req
        .app()
        .github_oauth
        .exchange_code(code)
        .request(http_client)
        .map_err(|err| err.chain(server_error("Error obtaining token")))?;
    let token = token.access_token();

    // Fetch the user info from GitHub using the access token we just got and create a user record
    let ghuser = req.app().github.current_user(token)?;
    let user = save_user_to_database(
        &ghuser,
        token.secret(),
        &req.app().emails,
        &*req.db_write()?,
    )?;

    let user_agent = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
        .unwrap_or_default();

    // Setup a session for the newly logged in user.
    let token = SecureToken::generate(crate::util::token::SecureTokenKind::Session);
    let session = PersistentSession::create(user.id, &token, req.remote_addr().ip(), &user_agent);
    session.insert(&*req.db_conn()?)?;

    // Setup session cookie.
    let secure = req.app().config.env() == Env::Production;
    req.cookies_mut().add(session_cookie(&token, secure));

    // Log in by setting a cookie and the middleware authentication
    // TODO(adsnaider): Remove.
    req.session_mut()
        .insert("user_id".to_string(), user.id.to_string());

    super::me::me(req)
}

fn save_user_to_database(
    user: &GithubUser,
    access_token: &str,
    emails: &Emails,
    conn: &PgConnection,
) -> AppResult<User> {
    NewUser::new(
        user.id,
        &user.login,
        user.name.as_deref(),
        user.avatar_url.as_deref(),
        access_token,
    )
    .create_or_update(user.email.as_deref(), emails, conn)
    .map_err(Into::into)
    .or_else(|e: Box<dyn AppError>| {
        // If we're in read only mode, we can't update their details
        // just look for an existing user
        if e.is::<ReadOnlyMode>() {
            users::table
                .filter(users::gh_id.eq(user.id))
                .first(conn)
                .optional()?
                .ok_or(e)
        } else {
            Err(e)
        }
    })
}

/// Handles the `DELETE /api/private/session` route.
pub fn logout(req: &mut dyn RequestExt) -> EndpointResult {
    // TODO(adsnaider): Remove this.
    req.session_mut().remove(&"user_id".to_string());
    if let Some(session_token) = req
        .cookies()
        .get(SESSION_COOKIE_NAME)
        .map(|cookie| cookie.value().to_string())
    {
        req.cookies_mut().remove(Cookie::named(SESSION_COOKIE_NAME));

        if let Ok(conn) = req.db_conn() {
            match PersistentSession::revoke_from_token(&conn, &session_token) {
                Ok(0) => {}
                Ok(1) => {}
                Ok(_count) => {}
                Err(_e) => {}
            }
        }
    }
    Ok(req.json(&true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg_connection() -> PgConnection {
        let database_url =
            dotenv::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set to run tests");
        PgConnection::establish(&database_url).unwrap()
    }

    #[test]
    fn gh_user_with_invalid_email_doesnt_fail() {
        let emails = Emails::new_in_memory();
        let conn = pg_connection();
        let gh_user = GithubUser {
            email: Some("String.Format(\"{0}.{1}@live.com\", FirstName, LastName)".into()),
            name: Some("My Name".into()),
            login: "github_user".into(),
            id: -1,
            avatar_url: None,
        };
        let result = save_user_to_database(&gh_user, "arbitrary_token", &emails, &conn);

        assert!(
            result.is_ok(),
            "Creating a User from a GitHub user failed when it shouldn't have, {:?}",
            result
        );
    }
}
