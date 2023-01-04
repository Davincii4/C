use bytes::Bytes;
use std::borrow::Cow;
use std::convert::TryInto;
use std::io::{Cursor, Read};

use conduit::{
    header::{HeaderValue, IntoHeaderName},
    Body, Extensions, HeaderMap, Method, Response, Uri,
};

pub trait ResponseExt {
    fn into_cow(self) -> Cow<'static, [u8]>;
}

impl ResponseExt for Response<Body> {
    /// Convert the request into a copy-on-write body
    fn into_cow(self) -> Cow<'static, [u8]> {
        use conduit::Body::*;

        match self.into_body() {
            Static(slice) => slice.into(),
            Owned(vec) => vec.into(),
        }
    }
}

pub struct MockRequest {
    request: conduit::Request<Cursor<Bytes>>,
}

impl MockRequest {
    pub fn new(method: Method, path: &str) -> MockRequest {
        let request = conduit::Request::builder()
            .method(&method)
            .uri(path)
            .body(Cursor::new(Bytes::new()))
            .unwrap();

        MockRequest { request }
    }

    pub fn with_query(&mut self, string: &str) -> &mut MockRequest {
        let path_and_query = format!("{}?{}", self.request.uri().path(), string);
        *self.request.uri_mut() = path_and_query.try_into().unwrap();
        self
    }

    pub fn with_body(&mut self, bytes: &[u8]) -> &mut MockRequest {
        *self.request.body_mut() = Cursor::new(bytes.to_vec().into());
        self
    }

    pub fn header<K>(&mut self, name: K, value: &str) -> &mut MockRequest
    where
        K: IntoHeaderName,
    {
        self.request
            .headers_mut()
            .insert(name, HeaderValue::from_str(value).unwrap());
        self
    }
}

impl conduit::RequestExt for MockRequest {
    fn method(&self) -> &Method {
        self.request.method()
    }

    fn uri(&self) -> &Uri {
        self.request.uri()
    }

    fn content_length(&self) -> Option<u64> {
        Some(self.request.body().get_ref().len() as u64)
    }

    fn headers(&self) -> &HeaderMap {
        self.request.headers()
    }

    fn body(&mut self) -> &mut dyn Read {
        self.request.body_mut()
    }

    fn extensions(&self) -> &Extensions {
        self.request.extensions()
    }
    fn extensions_mut(&mut self) -> &mut Extensions {
        self.request.extensions_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::MockRequest;

    use conduit::{header, Method, RequestExt};

    #[test]
    fn simple_request_test() {
        let mut req = MockRequest::new(Method::GET, "/");

        assert_eq!(req.method(), Method::GET);
        assert_eq!(req.uri(), "/");
        assert_eq!(req.content_length(), Some(0));
        assert_eq!(req.headers().len(), 0);
        let mut s = String::new();
        req.body().read_to_string(&mut s).expect("No body");
        assert_eq!(s, "".to_string());
    }

    #[test]
    fn request_body_test() {
        let mut req = MockRequest::new(Method::POST, "/articles");
        req.with_body(b"Hello world");

        assert_eq!(req.method(), Method::POST);
        assert_eq!(req.uri(), "/articles");
        let mut s = String::new();
        req.body().read_to_string(&mut s).expect("No body");
        assert_eq!(s, "Hello world".to_string());
        assert_eq!(req.content_length(), Some(11));
    }

    #[test]
    fn request_query_test() {
        let mut req = MockRequest::new(Method::POST, "/articles");
        req.with_query("foo=bar");

        assert_eq!(req.uri().query().expect("No query string"), "foo=bar");
    }

    #[test]
    fn request_headers() {
        let mut req = MockRequest::new(Method::POST, "/articles");
        req.header(header::USER_AGENT, "lulz");
        req.header(header::DNT, "1");

        assert_eq!(req.headers().len(), 2);
        assert_eq!(req.headers().get(header::USER_AGENT).unwrap(), "lulz");
        assert_eq!(req.headers().get(header::DNT).unwrap(), "1");
    }
}
