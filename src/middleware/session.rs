use crate::controllers::util::RequestPartsExt;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::SignedCookieJar;
use cookie::time::Duration;
use cookie::{Cookie, SameSite};
use http::Request;
use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

static COOKIE_NAME: &str = "cargo_session";
static MAX_AGE_DAYS: i64 = 90;

pub async fn attach_session<B>(
    jar: SignedCookieJar,
    mut req: Request<B>,
    next: Next<B>,
) -> Response {
    // Decode session cookie
    let data = jar.get(COOKIE_NAME).map(decode).unwrap_or_default();

    // Save decoded session data in request extension,
    // and keep an `Arc` clone for later
    let session = Arc::new(RwLock::new(Session::new(data)));
    req.extensions_mut().insert(session.clone());

    // Process the request
    let response = next.run(req).await;

    // Check if the session data was mutated
    let session = session.read().unwrap();
    if session.dirty {
        // Return response with additional `Set-Cookie` header
        let encoded = encode(&session.data);
        let cookie = Cookie::build(COOKIE_NAME, encoded)
            .http_only(true)
            .secure(true)
            .same_site(SameSite::Strict)
            .max_age(Duration::days(MAX_AGE_DAYS))
            .path("/")
            .finish();

        (jar.add(cookie), response).into_response()
    } else {
        response
    }
}

/// Request extension holding the session data
struct Session {
    data: HashMap<String, String>,
    dirty: bool,
}

impl Session {
    fn new(data: HashMap<String, String>) -> Self {
        Self { data, dirty: false }
    }
}

pub trait RequestSession {
    fn session_get(&self, key: &str) -> Option<String>;
    fn session_insert(&self, key: String, value: String) -> Option<String>;
    fn session_remove(&self, key: &str) -> Option<String>;
}

impl<T: RequestPartsExt> RequestSession for T {
    fn session_get(&self, key: &str) -> Option<String> {
        let session = self
            .extensions()
            .get::<Arc<RwLock<Session>>>()
            .expect("missing cookie session")
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        session.data.get(key).cloned()
    }

    fn session_insert(&self, key: String, value: String) -> Option<String> {
        let mut session = self
            .extensions()
            .get::<Arc<RwLock<Session>>>()
            .expect("missing cookie session")
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        session.dirty = true;
        session.data.insert(key, value)
    }

    fn session_remove(&self, key: &str) -> Option<String> {
        let mut session = self
            .extensions()
            .get::<Arc<RwLock<Session>>>()
            .expect("missing cookie session")
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        session.dirty = true;
        session.data.remove(key)
    }
}

pub fn decode(cookie: Cookie<'_>) -> HashMap<String, String> {
    let mut ret = HashMap::new();
    let bytes = base64::decode(cookie.value().as_bytes()).unwrap_or_default();
    let mut parts = bytes.split(|&a| a == 0xff);
    while let (Some(key), Some(value)) = (parts.next(), parts.next()) {
        if key.is_empty() {
            break;
        }
        if let (Ok(key), Ok(value)) = (std::str::from_utf8(key), std::str::from_utf8(value)) {
            ret.insert(key.to_string(), value.to_string());
        }
    }
    ret
}

pub fn encode(h: &HashMap<String, String>) -> String {
    let mut ret = Vec::new();
    for (i, (k, v)) in h.iter().enumerate() {
        if i != 0 {
            ret.push(0xff)
        }
        ret.extend(k.bytes());
        ret.push(0xff);
        ret.extend(v.bytes());
    }
    while ret.len() * 8 % 6 != 0 {
        ret.push(0xff);
    }
    base64::encode(&ret[..])
}
