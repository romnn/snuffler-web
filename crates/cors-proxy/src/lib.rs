//! Allowlisted streaming CORS proxy for browser-side HTTP requests.
//!
//! The browser asks for `/cors?url=<target>`.
//! A `GET` or `HEAD` to an allowlisted target is forwarded, and the response streams back with
//! CORS headers added; everything else gets a plain-text error.
//! A redirect is followed only to a target the allowlist has too.

mod allowlist;
mod headers;
mod metrics;

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode};
use reqwest::Url;

pub use self::allowlist::{AllowedHttpTarget, Allowlist, TargetError};
use self::headers::{add_cors_headers, forwardable_headers};
use self::metrics::{HandledRequest, OTHER_SERVER, Outcome, PendingUpstream, count_request};

/// The most redirects one proxied request follows, as many as reqwest's own default.
const MAX_REDIRECTS: usize = 10;

/// Proxies safe HTTP requests through a configured [`Allowlist`].
#[derive(Clone)]
pub struct Proxy {
    http_client: reqwest::Client,
    allowlist: Arc<Allowlist>,
}

impl Proxy {
    /// Creates a proxy forwarding to the targets in `allowlist`, with a client built from
    /// `http_client`.
    ///
    /// The client's redirect policy is replaced: it follows a redirect only to a target
    /// `allowlist` has, so a redirect cannot reach a host or path the allowlist leaves out.
    ///
    /// # Errors
    ///
    /// Returns the [`reqwest::Error`] of a client that does not build.
    pub fn new(
        http_client: reqwest::ClientBuilder,
        allowlist: Allowlist,
    ) -> Result<Self, reqwest::Error> {
        let allowlist = Arc::new(allowlist);
        let http_client = http_client
            .redirect(redirect_policy(Arc::clone(&allowlist)))
            .build()?;
        Ok(Self {
            http_client,
            allowlist,
        })
    }

    /// Handles one browser proxy request and maps failures into HTTP responses.
    ///
    /// Every request but a preflight counts towards `cors_proxy_requests`, by `outcome`
    /// (`forwarded`, `denied`, `invalid` or `upstream_failed`), upstream `server.address` and
    /// response status.
    /// One that reached the upstream also records how long the upstream took to answer in
    /// `cors_proxy_upstream_duration_ms`, and counts towards
    /// `cors_proxy_pending_upstream_requests` while it waits.
    #[tracing::instrument(name = "cors_request", skip_all, fields(method = %request.method(), upstream))]
    pub async fn handle(&self, request: Request<Body>) -> Response<Body> {
        if request.method() == Method::OPTIONS {
            return cors_preflight_response();
        }
        let started = Instant::now();
        let (result, server_address) = match self.allowed_target(&request) {
            Ok(target) => {
                let server_address = target.host_str().unwrap_or(OTHER_SERVER).to_owned();
                let result = {
                    let _pending = PendingUpstream::start(&server_address);
                    self.forward(request, target).await
                };
                (result, server_address)
            }
            Err(err) => (Err(err), OTHER_SERVER.to_owned()),
        };
        let elapsed = started.elapsed();
        let (response, outcome, upstream_error) = match result {
            Ok(response) => (response, Outcome::Forwarded, None),
            Err(err) => {
                let message = error_chain(&err);
                tracing::warn!(error = %message, "proxy request failed");
                let response = text_response(err.status(), format!("{message}\n"));
                (response, err.outcome(), err.upstream_error())
            }
        };
        count_request(&HandledRequest {
            outcome,
            upstream_error,
            server_address: &server_address,
            status: response.status(),
            elapsed,
        });
        response
    }

    /// The request's target, when its method is safe and the allowlist has it.
    fn allowed_target(&self, request: &Request<Body>) -> Result<Url, ProxyError> {
        if !matches!(*request.method(), Method::GET | Method::HEAD) {
            return Err(ProxyError::MethodNotAllowed(request.method().clone()));
        }
        let target = target_url_from_query(request.uri().query())?;
        // Only the parts the allowlist compares, so a query string never reaches a trace.
        tracing::Span::current().record(
            "upstream",
            format!(
                "{}://{}{}",
                target.scheme(),
                target.host_str().unwrap_or_default(),
                target.path()
            ),
        );
        self.allowlist.check(&target)?;
        Ok(target)
    }

    async fn forward(
        &self,
        request: Request<Body>,
        target: Url,
    ) -> Result<Response<Body>, ProxyError> {
        // Forward the request, minus hop-by-hop headers and browser credentials.
        // Only GET and HEAD get here, so no body is sent, which also lets the client follow a
        // 307 or 308 redirect: it cannot replay a streamed body.
        let request_headers = forwardable_headers(request.headers());
        let mut upstream_request = self.http_client.request(request.method().clone(), target);
        for (name, value) in request_headers {
            upstream_request = upstream_request.header(name, value);
        }
        // reqwest's errors name the full URL, query included, which can carry a signed download
        // URL's token, and they reach the log and the browser's error body.
        let upstream_response = upstream_request
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;

        // Stream the response back with the same filtering, plus CORS headers
        let mut response = Response::builder().status(upstream_response.status());
        for (name, value) in forwardable_headers(upstream_response.headers()) {
            response = response.header(name, value);
        }
        let response = response.body(Body::from_stream(upstream_response.bytes_stream()))?;
        Ok(add_cors_headers(response))
    }
}

/// Follows a redirect only to a target `allowlist` has, and at most [`MAX_REDIRECTS`] of them.
///
/// The allowlist is checked once for the requested URL, so without this a redirect from an
/// allowed target would reach any path or host.
fn redirect_policy(allowlist: Arc<Allowlist>) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.error(TooManyRedirects);
        }
        match allowlist.check(attempt.url()) {
            Ok(()) => attempt.follow(),
            Err(err) => attempt.error(err),
        }
    })
}

/// A request redirected more than [`MAX_REDIRECTS`] times.
#[derive(Debug, thiserror::Error)]
#[error("more than {MAX_REDIRECTS} redirects")]
struct TooManyRedirects;

/// Why a proxied request failed.
///
/// Every variant maps to one HTTP status through [`ProxyError::status`].
#[derive(Debug, thiserror::Error)]
enum ProxyError {
    #[error("method is not allowed: {0}")]
    MethodNotAllowed(Method),
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
            Self::MethodNotAllowed(_) => StatusCode::METHOD_NOT_ALLOWED,
            Self::NotAllowed(_) => StatusCode::FORBIDDEN,
            Self::Upstream(_) | Self::Response(_) => StatusCode::BAD_GATEWAY,
        }
    }

    fn outcome(&self) -> Outcome {
        match self {
            Self::MethodNotAllowed(_)
            | Self::MissingTargetUrl
            | Self::InvalidTargetUrl(_)
            | Self::UnsupportedScheme(_) => Outcome::Invalid,
            Self::NotAllowed(_) => Outcome::Denied,
            Self::Upstream(_) | Self::Response(_) => Outcome::UpstreamFailed,
        }
    }

    /// The `error.type` of a failed upstream request.
    fn upstream_error(&self) -> Option<&'static str> {
        match self {
            Self::Upstream(err) if err.is_timeout() => Some("timeout"),
            Self::Upstream(err) if err.is_connect() => Some("connect"),
            Self::Upstream(err) if err.is_redirect() => Some("redirect"),
            Self::Upstream(_) => Some("request"),
            Self::Response(_) => Some("response"),
            Self::MethodNotAllowed(_)
            | Self::MissingTargetUrl
            | Self::InvalidTargetUrl(_)
            | Self::UnsupportedScheme(_)
            | Self::NotAllowed(_) => None,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::response::Redirect;
    use axum::routing::get;
    use color_eyre::eyre::{self, bail};
    use tokio::net::TcpListener;

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
        let url = target_url_from_query(Some("url=https%3A%2F%2Fdata.example.org%2Fapi%2Fquery"))?;
        assert_eq!(url.as_str(), "https://data.example.org/api/query");
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

    /// Serves an upstream on a free local port: `/hop` redirects to the allowed `/data`, `/escape`
    /// to `/secret`, which the allowlist leaves out, and `/secret` counts the requests it gets.
    async fn redirecting_upstream(secret_hits: Arc<AtomicUsize>) -> eyre::Result<String> {
        let app = Router::new()
            .route("/hop", get(|| async { Redirect::temporary("/data") }))
            .route("/data", get(|| async { "data" }))
            .route("/escape", get(|| async { Redirect::temporary("/secret") }))
            .route(
                "/secret",
                get(move || async move {
                    secret_hits.fetch_add(1, Ordering::SeqCst);
                    "secret"
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        tokio::spawn(async move { axum::serve(listener, app).await });
        Ok(base)
    }

    async fn proxy_get(proxy: &Proxy, target: &str) -> eyre::Result<(StatusCode, String)> {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("url", target)
            .finish();
        let request = Request::get(format!("/cors?{query}")).body(Body::empty())?;
        let response = proxy.handle(request).await;
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        Ok((status, String::from_utf8(body.to_vec())?))
    }

    #[tokio::test]
    async fn follows_redirects_only_within_the_allowlist() -> eyre::Result<()> {
        let secret_hits = Arc::new(AtomicUsize::new(0));
        let base = redirecting_upstream(Arc::clone(&secret_hits)).await?;
        let allowlist = Allowlist::new(
            ["/hop", "/data", "/escape"]
                .into_iter()
                .map(|path| AllowedHttpTarget::parse(&format!("{base}{path}")))
                .collect::<Result<Vec<_>, _>>()?,
        );
        let proxy = Proxy::new(reqwest::Client::builder(), allowlist)?;

        // A redirect to another allowed target is followed, and its response comes back.
        let (status, body) = proxy_get(&proxy, &format!("{base}/hop")).await?;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "data"));

        // A redirect out of the allowlist fails the request before it reaches the target, and the
        // error names neither URL's query, where a signed download URL keeps its token.
        let (status, body) = proxy_get(&proxy, &format!("{base}/escape?token=SENTINEL")).await?;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("target URL is not allowed"), "{body}");
        assert!(!body.contains("SENTINEL"), "{body}");
        assert_eq!(secret_hits.load(Ordering::SeqCst), 0);
        Ok(())
    }
}
