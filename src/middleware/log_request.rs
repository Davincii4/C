//! Log all requests in a format similar to Heroku's router, but with additional
//! information that we care about like User-Agent

use super::prelude::*;
use crate::util::request_header;

use conduit::RequestExt;

use crate::middleware::normalize_path::OriginalPath;
use crate::middleware::response_timing::ResponseTime;
use http::{header, Method, StatusCode};
use std::cell::RefCell;
use std::fmt::{self, Display, Formatter};

const SLOW_REQUEST_THRESHOLD_MS: u64 = 1000;

// A thread local is used instead of a request extension to avoid the need to pass the request
// object everywhere in the codebase. When migrating to async this will need to be moved to an
// async-equivalent, as thread locals misbehave in async contexes.
thread_local! {
    static CUSTOM_METADATA: RefCell<Vec<(&'static str, String)>> = RefCell::new(Vec::new());
}

#[derive(Default)]
pub(super) struct LogRequests();

impl Middleware for LogRequests {
    fn before(&self, _: &mut dyn RequestExt) -> BeforeResult {
        // Remove any metadata set by the previous task before processing any new request.
        CUSTOM_METADATA.with(|metadata| metadata.borrow_mut().clear());

        Ok(())
    }

    fn after(&self, req: &mut dyn RequestExt, res: AfterResult) -> AfterResult {
        RequestLine::new(req, &res).log();

        res
    }
}

pub fn add_custom_metadata<V: Display>(key: &'static str, value: V) {
    CUSTOM_METADATA.with(|metadata| metadata.borrow_mut().push((key, value.to_string())));
    sentry::configure_scope(|scope| scope.set_extra(key, value.to_string().into()));
}

#[cfg(test)]
pub(crate) fn get_log_message(key: &'static str) -> String {
    CUSTOM_METADATA.with(|metadata| {
        for (k, v) in &*metadata.borrow() {
            if key == *k {
                return v.clone();
            }
        }
        panic!("expected log message for {} not found", key);
    })
}

struct RequestLine<'r> {
    req: &'r dyn RequestExt,
    res: &'r AfterResult,
    status: StatusCode,
}

impl<'a> RequestLine<'a> {
    fn new(request: &'a dyn RequestExt, response: &'a AfterResult) -> Self {
        let status = response.as_ref().map(|res| res.status());
        let status = status.unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        RequestLine {
            req: request,
            res: response,
            status,
        }
    }

    fn log(&self) {
        if self.status.is_server_error() {
            error!(target: "http", "{self}");
        } else {
            info!(target: "http", "{self}");
        };
    }
}

impl Display for RequestLine<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut line = LogLine::new(f);

        // The download endpoint is our most requested endpoint by 1-2 orders of
        // magnitude. Since we pay per logged GB we try to reduce the amount of
        // bytes per log line for this endpoint.

        let is_download_endpoint = self.req.path().ends_with("/download");
        let is_download_redirect = is_download_endpoint && self.status.is_redirection();

        let method = self.req.method();
        if !is_download_redirect || method != Method::GET {
            line.add_field("method", method)?;
        }

        line.add_quoted_field("path", FullPath(self.req))?;

        if !is_download_redirect {
            line.add_field("request_id", request_header(self.req, "x-request-id"))?;
        }

        line.add_quoted_field("fwd", request_header(self.req, "x-real-ip"))?;

        let response_time = self.req.extensions().get::<ResponseTime>();
        if let Some(response_time) = response_time {
            if !is_download_redirect || response_time.as_millis() > 0 {
                line.add_field("service", response_time)?;
            }
        }

        if !is_download_redirect {
            line.add_field("status", self.status.as_str())?;
        }

        line.add_quoted_field("user_agent", request_header(self.req, header::USER_AGENT))?;

        CUSTOM_METADATA.with(|metadata| {
            for (key, value) in &*metadata.borrow() {
                line.add_quoted_field(key, value)?;
            }
            fmt::Result::Ok(())
        })?;

        if let Err(err) = self.res {
            line.add_quoted_field("error", err)?;
        }

        if let Some(response_time) = response_time {
            if response_time.as_millis() > SLOW_REQUEST_THRESHOLD_MS {
                line.add_marker("SLOW REQUEST")?;
            }
        }

        Ok(())
    }
}

struct FullPath<'a>(&'a dyn RequestExt);

impl<'a> Display for FullPath<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let request = self.0;

        let original_path = request.extensions().get::<OriginalPath>();
        let path = original_path
            .map(|p| p.0.as_str())
            .unwrap_or_else(|| request.path());

        write!(f, "{}", path)?;

        if let Some(q_string) = request.query_string() {
            write!(f, "?{}", q_string)?;
        }
        Ok(())
    }
}

struct LogLine<'f, 'g> {
    f: &'f mut Formatter<'g>,
    first: bool,
}

impl<'f, 'g> LogLine<'f, 'g> {
    fn new(f: &'f mut Formatter<'g>) -> Self {
        Self { f, first: true }
    }

    fn add_field<K: Display, V: Display>(&mut self, key: K, value: V) -> fmt::Result {
        self.start_item()?;

        key.fmt(self.f)?;
        self.f.write_str("=")?;
        value.fmt(self.f)?;

        Ok(())
    }

    fn add_quoted_field<K: Display, V: Display>(&mut self, key: K, value: V) -> fmt::Result {
        self.start_item()?;

        key.fmt(self.f)?;
        self.f.write_str("=\"")?;
        value.fmt(self.f)?;
        self.f.write_str("\"")?;

        Ok(())
    }

    fn add_marker<M: Display>(&mut self, marker: M) -> fmt::Result {
        self.start_item()?;

        marker.fmt(self.f)?;

        Ok(())
    }

    fn start_item(&mut self) -> fmt::Result {
        if !self.first {
            self.f.write_str(" ")?;
        }
        self.first = false;
        Ok(())
    }
}
