//! Header handling: what crosses the proxy in either direction, and the CORS headers it adds.

use std::collections::HashSet;

use axum::body::Body;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE, AUTHORIZATION, CONNECTION,
    CONTENT_LENGTH, COOKIE, HOST, ORIGIN, SET_COOKIE, TRANSFER_ENCODING,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Response};

/// Lets any page read the response, including from a cross-origin-isolated app.
pub(crate) fn add_cors_headers(mut response: Response<Body>) -> Response<Body> {
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

/// The headers worth passing on: everything except hop-by-hop headers and credentials.
pub(crate) fn forwardable_headers(headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)> {
    let connection_tokens = headers
        .get(CONNECTION)
        .and_then(|value| value.to_str().ok())
        .map(connection_header_tokens)
        .unwrap_or_default();

    headers
        .iter()
        .filter(|(name, _)| !is_unforwardable_header(name, &connection_tokens))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// The header names a `Connection` header lists, which are hop-by-hop too.
fn connection_header_tokens(value: &str) -> HashSet<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn is_unforwardable_header(name: &HeaderName, connection_tokens: &HashSet<String>) -> bool {
    // Hop-by-hop headers describe one connection; the upstream request opens its own.
    let is_connection_specific = matches!(
        *name,
        HOST | CONNECTION | CONTENT_LENGTH | TRANSFER_ENCODING
    ) || matches!(
        name.as_str(),
        "keep-alive" | "proxy-authenticate" | "proxy-authorization" | "te" | "trailer" | "upgrade"
    ) || connection_tokens.contains(name.as_str());

    // Browser credentials and origin must not leak to the upstream, nor come back from it.
    let is_credential = matches!(*name, ORIGIN | AUTHORIZATION | COOKIE | SET_COOKIE);

    is_connection_specific || is_credential
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwardable_headers_drop_browser_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        headers.insert(COOKIE, HeaderValue::from_static("sid=secret"));
        headers.insert(SET_COOKIE, HeaderValue::from_static("sid=secret"));
        headers.insert(
            HeaderName::from_static("x-snuffler-test"),
            HeaderValue::from_static("kept"),
        );

        let forwarded = forwardable_headers(&headers);
        assert!(
            forwarded
                .iter()
                .all(|(name, _)| !matches!(*name, AUTHORIZATION | COOKIE | SET_COOKIE))
        );
        assert!(
            forwarded
                .iter()
                .any(|(name, value)| name.as_str() == "x-snuffler-test" && value == "kept")
        );
    }
}
