//! Allowlisted streaming CORS proxy for browser-side FDSN requests.
//!
//! The browser asks for `/cors?url=<target>`.
//! A `GET` or `HEAD` to an allowlisted target is forwarded, and the response streams back with
//! CORS headers added; everything else gets a plain-text error.

mod allowlist;
mod headers;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode};
use reqwest::Url;

pub use self::allowlist::{AllowedHttpTarget, Allowlist, TargetError};
use self::headers::{add_cors_headers, forwardable_headers};

/// Proxies safe HTTP requests through a configured [`Allowlist`].
#[derive(Clone)]
pub struct Proxy {
    http_client: reqwest::Client,
    allowlist: Arc<Allowlist>,
}

impl Proxy {
    /// Creates a proxy forwarding to the targets in `allowlist`.
    #[must_use]
    pub fn new(http_client: reqwest::Client, allowlist: Allowlist) -> Self {
        Self {
            http_client,
            allowlist: Arc::new(allowlist),
        }
    }

    /// Handles one browser proxy request and maps failures into HTTP responses.
    pub async fn handle(&self, request: Request<Body>) -> Response<Body> {
        if request.method() == Method::OPTIONS {
            return cors_preflight_response();
        }
        if !matches!(*request.method(), Method::GET | Method::HEAD) {
            return text_response(
                StatusCode::METHOD_NOT_ALLOWED,
                format!("method is not allowed: {}\n", request.method()),
            );
        }

        match self.forward(request).await {
            Ok(response) => response,
            Err(err) => text_response(err.status(), format!("{}\n", error_chain(&err))),
        }
    }

    async fn forward(&self, mut request: Request<Body>) -> Result<Response<Body>, ProxyError> {
        let target = target_url_from_query(request.uri().query())?;
        self.allowlist.check(&target)?;

        // Forward the request, minus hop-by-hop headers and browser credentials
        let request_headers = forwardable_headers(request.headers());
        let body = std::mem::take(request.body_mut());
        let mut upstream_request = self
            .http_client
            .request(request.method().clone(), target)
            .body(reqwest::Body::wrap_stream(body.into_data_stream()));
        for (name, value) in request_headers {
            upstream_request = upstream_request.header(name, value);
        }
        let upstream_response = upstream_request.send().await?;

        // Stream the response back with the same filtering, plus CORS headers
        let mut response = Response::builder().status(upstream_response.status());
        for (name, value) in forwardable_headers(upstream_response.headers()) {
            response = response.header(name, value);
        }
        let response = response.body(Body::from_stream(upstream_response.bytes_stream()))?;
        Ok(add_cors_headers(response))
    }
}

/// Why a proxied request failed.
///
/// Every variant maps to one HTTP status through [`ProxyError::status`].
#[derive(Debug, thiserror::Error)]
enum ProxyError {
    #[error("missing url query parameter")]
    MissingTargetUrl,
    #[error("invalid target URL")]
    InvalidTargetUrl(#[source] url::ParseError),
    #[error("unsupported URL scheme: {0}")]
    UnsupportedScheme(String),
    #[error("target URL is not allowed: {0}")]
    NotAllowed(String),
    #[error("upstream request failed")]
    Upstream(#[from] reqwest::Error),
    #[error("failed to build the proxied response")]
    Response(#[from] axum::http::Error),
}

impl ProxyError {
    fn status(&self) -> StatusCode {
        match self {
            Self::MissingTargetUrl | Self::InvalidTargetUrl(_) | Self::UnsupportedScheme(_) => {
                StatusCode::BAD_REQUEST
            }
            Self::NotAllowed(_) => StatusCode::FORBIDDEN,
            Self::Upstream(_) | Self::Response(_) => StatusCode::BAD_GATEWAY,
        }
    }
}

/// Renders `err` and its sources as `outer: inner: …`.
///
/// The plain-text response body is all the browser sees, so it carries the whole chain.
fn error_chain(err: &ProxyError) -> String {
    std::iter::successors(Some(err as &dyn std::error::Error), |&err| err.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

fn cors_preflight_response() -> Response<Body> {
    add_cors_headers(
        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty())),
    )
}

fn text_response(status: StatusCode, body: String) -> Response<Body> {
    let response = Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()));
    add_cors_headers(response)
}

/// Reads the target from the `url` query parameter and accepts only HTTP(S).
fn target_url_from_query(query: Option<&str>) -> Result<Url, ProxyError> {
    let target = query
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
        .find_map(|(key, value)| (key == "url").then_some(value.into_owned()))
        .ok_or(ProxyError::MissingTargetUrl)?;
    let url = Url::parse(&target).map_err(ProxyError::InvalidTargetUrl)?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        scheme => Err(ProxyError::UnsupportedScheme(scheme.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre::{self, bail};

    use super::*;

    #[test]
    fn rejects_missing_target() {
        assert!(matches!(
            target_url_from_query(None),
            Err(ProxyError::MissingTargetUrl)
        ));
    }

    #[test]
    fn accepts_http_target() -> eyre::Result<()> {
        let url = target_url_from_query(Some(
            "url=https%3A%2F%2Fgeofon.gfz.de%2Ffdsnws%2Fdataselect%2F1%2Fquery",
        ))?;
        assert_eq!(
            url.as_str(),
            "https://geofon.gfz.de/fdsnws/dataselect/1/query"
        );
        Ok(())
    }

    #[test]
    fn rejects_non_http_target() {
        let result = target_url_from_query(Some("url=file%3A%2F%2F%2Fetc%2Fpasswd"));
        assert!(matches!(result, Err(ProxyError::UnsupportedScheme(scheme)) if scheme == "file"));
    }

    #[test]
    fn error_body_carries_the_whole_chain() -> eyre::Result<()> {
        let Err(err) = target_url_from_query(Some("url=not%20a%20url")) else {
            bail!("a relative URL must be rejected");
        };

        // The browser sees the cause behind the summary, and the request is the client's fault.
        assert_eq!(
            error_chain(&err),
            "invalid target URL: relative URL without a base"
        );
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        Ok(())
    }
}
