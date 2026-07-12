//! Allowlisted streaming CORS proxy for browser-side FDSN requests.

use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Body;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE, AUTHORIZATION, CONNECTION,
    CONTENT_LENGTH, COOKIE, HOST, ORIGIN, SET_COOKIE, TRANSFER_ENCODING,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode};
use futures_util::TryStreamExt;
use reqwest::Url;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Proxies safe HTTP requests through a configured [`Allowlist`].
#[derive(Clone)]
pub struct Proxy {
    http: reqwest::Client,
    allowlist: Arc<Allowlist>,
}

impl Proxy {
    /// Creates a proxy with the built-in FDSN target allowlist.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self::new_with_allowlist(http, Allowlist::default())
    }

    /// Creates a proxy with an explicit target allowlist.
    #[must_use]
    pub fn new_with_allowlist(http: reqwest::Client, allowlist: Allowlist) -> Self {
        Self {
            http,
            allowlist: Arc::new(allowlist),
        }
    }

    /// Handles one browser proxy request and maps failures into HTTP responses.
    pub async fn handle(&self, req: Request<Body>) -> Response<Body> {
        if req.method() == Method::OPTIONS {
            return cors_preflight();
        }
        if !matches!(*req.method(), Method::GET | Method::HEAD) {
            return text_response(
                StatusCode::METHOD_NOT_ALLOWED,
                format!("method is not allowed: {}\n", req.method()),
            );
        }

        match self.proxy(req).await {
            Ok(response) => response,
            Err(ProxyError::Forbidden(message)) => {
                text_response(StatusCode::FORBIDDEN, format!("{message}\n"))
            }
            Err(ProxyError::BadRequest(message)) => {
                text_response(StatusCode::BAD_REQUEST, format!("{message}\n"))
            }
            Err(ProxyError::Upstream(err)) => {
                text_response(StatusCode::BAD_GATEWAY, format!("proxy error: {err}\n"))
            }
        }
    }

    async fn proxy(&self, mut req: Request<Body>) -> Result<Response<Body>, ProxyError> {
        let target = target_url(req.uri().query(), req.headers())?;
        self.allowlist.check(&target)?;
        let method = reqwest::Method::from_bytes(req.method().as_str().as_bytes())
            .map_err(|err| ProxyError::BadRequest(err.to_string()))?;
        let filtered_request_headers = filtered_headers(req.headers());
        let body = std::mem::take(req.body_mut());
        let mut upstream = self
            .http
            .request(method, target)
            .body(reqwest::Body::wrap_stream(body.into_data_stream()));

        for (name, value) in filtered_request_headers {
            upstream = upstream.header(name, value);
        }

        let upstream = upstream.send().await.map_err(ProxyError::from)?;
        let status = upstream.status();
        let response_headers = filtered_headers(upstream.headers());
        let stream = upstream
            .bytes_stream()
            .map_err(|err| -> BoxError { Box::new(err) });
        let mut builder = Response::builder().status(status);

        for (name, value) in response_headers {
            builder = builder.header(name, value);
        }
        Ok(add_cors_headers(
            builder
                .body(Body::from_stream(stream))
                .map_err(|err| ProxyError::Upstream(Box::new(err)))?,
        ))
    }
}

#[derive(Debug)]
enum ProxyError {
    BadRequest(String),
    Forbidden(String),
    Upstream(BoxError),
}

impl From<BoxError> for ProxyError {
    fn from(err: BoxError) -> Self {
        Self::Upstream(err)
    }
}

impl From<reqwest::Error> for ProxyError {
    fn from(err: reqwest::Error) -> Self {
        Self::Upstream(Box::new(err))
    }
}

impl From<reqwest::header::InvalidHeaderValue> for ProxyError {
    fn from(err: reqwest::header::InvalidHeaderValue) -> Self {
        Self::Upstream(Box::new(err))
    }
}

fn cors_preflight() -> Response<Body> {
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

fn add_cors_headers(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(
        ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,HEAD,OPTIONS"),
    );
    headers.insert(ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("*"));
    headers.insert(ACCESS_CONTROL_EXPOSE_HEADERS, HeaderValue::from_static("*"));
    headers.insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("cross-origin"),
    );
    response
}

/// Exact HTTP targets accepted by a [`Proxy`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Allowlist {
    rules: Vec<AllowedHttpTarget>,
}

impl Allowlist {
    /// Creates an allowlist containing only `rules`.
    #[must_use]
    pub fn new(rules: Vec<AllowedHttpTarget>) -> Self {
        Self { rules }
    }

    /// Extends the built-in FDSN targets with deployment-specific rules.
    pub fn with_default_rules<I>(extra_rules: I) -> Self
    where
        I: IntoIterator<Item = AllowedHttpTarget>,
    {
        let mut rules = default_allowed_targets();
        rules.extend(extra_rules);
        Self { rules }
    }

    fn check(&self, url: &Url) -> Result<(), ProxyError> {
        if self.rules.iter().any(|rule| rule.matches(url)) {
            Ok(())
        } else {
            Err(ProxyError::Forbidden(format!(
                "target URL is not allowed: {}://{}{}",
                url.scheme(),
                url.host_str().unwrap_or_default(),
                url.path()
            )))
        }
    }
}

impl Default for Allowlist {
    fn default() -> Self {
        Self {
            rules: default_allowed_targets(),
        }
    }
}

/// One allowed scheme, host, port, and exact path tuple.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowedHttpTarget {
    scheme: String,
    host: String,
    port: u16,
    path: String,
}

impl AllowedHttpTarget {
    /// Creates a rule using the scheme's default port.
    pub fn new(
        scheme: impl Into<String>,
        host: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        let scheme = scheme.into().to_ascii_lowercase();
        let port = default_port(&scheme).unwrap_or(443);
        Self {
            scheme,
            host: host.into().to_ascii_lowercase(),
            port,
            path: path.into(),
        }
    }

    /// Creates a rule with an explicit port.
    pub fn new_with_port(
        scheme: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        path: impl Into<String>,
    ) -> Self {
        Self {
            scheme: scheme.into().to_ascii_lowercase(),
            host: host.into().to_ascii_lowercase(),
            port,
            path: path.into(),
        }
    }

    /// Parses an absolute URL into an exact target rule.
    ///
    /// # Errors
    ///
    /// Returns an error when the value lacks a host or a known or explicit port.
    pub fn parse(value: &str) -> Result<Self, BoxError> {
        let url = Url::parse(value)?;
        let host = url
            .host_str()
            .ok_or_else(|| "allowed URL target must include a host".to_owned())?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| "allowed URL target must have a known or explicit port".to_owned())?;
        Ok(Self::new_with_port(url.scheme(), host, port, url.path()))
    }

    fn matches(&self, url: &Url) -> bool {
        url.scheme() == self.scheme
            && url.host_str().map(str::to_ascii_lowercase).as_deref() == Some(self.host.as_str())
            && url.port_or_known_default() == Some(self.port)
            && url.path() == self.path
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

fn default_allowed_targets() -> Vec<AllowedHttpTarget> {
    const HOSTS: &[&str] = &[
        "geofon.gfz.de",
        "service.earthscope.org",
        "service.iris.edu",
        "earthquake.usgs.gov",
        "www.orfeus-eu.org",
        "webservices.ingv.it",
        "ws.resif.fr",
        "eida.bgr.de",
    ];
    const PATHS: &[&str] = &[
        "/fdsnws/dataselect/1/query",
        "/fdsnws/event/1/query",
        "/fdsnws/station/1/query",
    ];
    HOSTS
        .iter()
        .flat_map(|host| {
            PATHS
                .iter()
                .map(move |path| AllowedHttpTarget::new("https", *host, *path))
        })
        .collect()
}

fn target_url(query: Option<&str>, headers: &HeaderMap) -> Result<Url, ProxyError> {
    let target = query
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
        .find_map(|(key, value)| (key == "url").then_some(value.into_owned()))
        .or_else(|| {
            headers
                .get("x-snuffler-proxy-url")
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| ProxyError::BadRequest("missing url query parameter".to_owned()))?;
    let url = Url::parse(&target).map_err(|err| ProxyError::BadRequest(err.to_string()))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        scheme => Err(ProxyError::BadRequest(format!(
            "unsupported URL scheme: {scheme}"
        ))),
    }
}

fn filtered_headers(headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)> {
    let connection_tokens = headers
        .get(CONNECTION)
        .and_then(|value| value.to_str().ok())
        .map(connection_header_tokens)
        .unwrap_or_default();

    headers
        .iter()
        .filter(|(name, _)| !is_hop_by_hop_header(name, &connection_tokens))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn connection_header_tokens(value: &str) -> HashSet<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn is_hop_by_hop_header(name: &HeaderName, connection_tokens: &HashSet<String>) -> bool {
    matches!(
        *name,
        HOST | CONNECTION
            | CONTENT_LENGTH
            | TRANSFER_ENCODING
            | ORIGIN
            | AUTHORIZATION
            | COOKIE
            | SET_COOKIE
    ) || matches!(
        name.as_str(),
        "keep-alive" | "proxy-authenticate" | "proxy-authorization" | "te" | "trailer" | "upgrade"
    ) || connection_tokens.contains(name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_target() {
        assert!(target_url(None, &HeaderMap::new()).is_err());
    }

    #[test]
    fn accepts_http_target() {
        let parsed = target_url(
            Some("url=https%3A%2F%2Fgeofon.gfz.de%2Ffdsnws%2Fdataselect%2F1%2Fquery"),
            &HeaderMap::new(),
        )
        .map(|url| url.to_string());
        assert_eq!(
            parsed.as_deref().ok(),
            Some("https://geofon.gfz.de/fdsnws/dataselect/1/query")
        );
    }

    #[test]
    fn rejects_non_http_target() {
        assert!(target_url(Some("url=file%3A%2F%2F%2Fetc%2Fpasswd"), &HeaderMap::new()).is_err());
    }

    #[test]
    fn default_allowlist_accepts_fdsn_query() -> Result<(), BoxError> {
        let url = Url::parse("https://geofon.gfz.de/fdsnws/dataselect/1/query?net=GE")?;
        assert!(Allowlist::default().check(&url).is_ok());
        Ok(())
    }

    #[test]
    fn default_allowlist_rejects_other_pages_on_allowed_host() -> Result<(), BoxError> {
        let url = Url::parse("https://geofon.gfz.de/index.html")?;
        assert!(Allowlist::default().check(&url).is_err());
        Ok(())
    }

    #[test]
    fn default_allowlist_rejects_other_ports_on_allowed_host() -> Result<(), BoxError> {
        let url = Url::parse("https://geofon.gfz.de:444/fdsnws/dataselect/1/query?net=GE")?;
        assert!(Allowlist::default().check(&url).is_err());
        Ok(())
    }

    #[test]
    fn default_allowlist_rejects_other_hosts() -> Result<(), BoxError> {
        let url = Url::parse("https://example.org/fdsnws/dataselect/1/query")?;
        assert!(Allowlist::default().check(&url).is_err());
        Ok(())
    }

    #[test]
    fn filtered_headers_drop_browser_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        headers.insert(COOKIE, HeaderValue::from_static("sid=secret"));
        headers.insert(SET_COOKIE, HeaderValue::from_static("sid=secret"));
        headers.insert(
            HeaderName::from_static("x-snuffler-test"),
            HeaderValue::from_static("kept"),
        );

        let filtered = filtered_headers(&headers);
        assert!(
            filtered
                .iter()
                .all(|(name, _)| !matches!(*name, AUTHORIZATION | COOKIE | SET_COOKIE))
        );
        assert!(
            filtered
                .iter()
                .any(|(name, value)| name.as_str() == "x-snuffler-test" && value == "kept")
        );
    }
}
