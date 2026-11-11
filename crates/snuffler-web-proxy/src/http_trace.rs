//! The request span and log line of tower-http's [`TraceLayer`](tower_http::trace::TraceLayer).
//!
//! Each proxied request gets one `http_request` span, named and attributed after the
//! OpenTelemetry HTTP semantic conventions, and one log line when its response starts.
//! It is a `tracing` span, so the stderr log and the OTLP export see the same span.
//! While spans are exported, it also carries their `trace_id`, so every stderr line logged
//! inside a request can be looked up in the trace backend.

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, MatchedPath};
use axum::http::{Request, Response};
use opentelemetry::trace::{TraceContextExt as _, TraceId};
use opentelemetry_http::HeaderExtractor;
use tracing::{Span, field};
use tracing_opentelemetry::{OpenTelemetrySpanExt as _, SetParentError};

/// Opens the span for one request, continuing the trace of an incoming W3C `traceparent`.
///
/// It names the route template, never the URI: the query carries the proxied URL, which would
/// make every span name unique.
/// The client address is the peer of the TCP connection, so behind a reverse proxy it is that
/// proxy's.
pub(crate) fn request_span(request: &Request<Body>) -> Span {
    let method = request.method();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("", MatchedPath::as_str);
    let client_address = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(address)| address.ip().to_string());
    let span = tracing::info_span!(
        "http_request",
        otel.name = %format_args!("{method} {route}"),
        otel.kind = "server",
        otel.status_code = field::Empty,
        http.request.method = %method,
        http.route = route,
        client.address = client_address.as_deref(),
        http.response.status_code = field::Empty,
        trace_id = field::Empty,
    );

    // The parent comes from the headers alone, on a fresh context, never from whatever
    // context happens to be current: an unsampled one there would drop every trace that did
    // not arrive with a `traceparent`.
    let parent = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract_with_context(
            &opentelemetry::Context::new(),
            &HeaderExtractor(request.headers()),
        )
    });
    // Without span export, or with the span filtered out, there is no trace to continue.
    if let Err(SetParentError::AlreadyStarted) = span.set_parent(parent) {
        tracing::warn!("request span started before its parent was set");
    }
    span
}

/// Records the span's OpenTelemetry trace id, which exists only while spans are exported.
pub(crate) fn record_trace_id(_request: &Request<Body>, span: &Span) {
    let trace_id = span.context().span().span_context().trace_id();
    if trace_id != TraceId::INVALID {
        span.record("trace_id", field::display(trace_id));
    }
}

/// Records the response status on the span and logs the request.
pub(crate) fn record_response(response: &Response<Body>, latency: Duration, span: &Span) {
    let status = response.status();
    span.record("http.response.status_code", status.as_u16());
    // The semantic conventions mark a server span failed only for a 5xx; a 4xx is the
    // client's error.
    if status.is_server_error() {
        span.record("otel.status_code", "ERROR");
    }
    tracing::info!(?latency, "finished request");
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre;
    use opentelemetry::trace::{SpanContext, TracerProvider as _};
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    /// Opens the span for `request` with span export on, inside an unrelated unsampled context,
    /// and returns the span's trace context.
    fn traced_span_context(request: &Request<Body>) -> SpanContext {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
        let provider = SdkTracerProvider::builder().build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        let _placeholder = opentelemetry::Context::new()
            .with_remote_span_context(SpanContext::empty_context())
            .attach();
        tracing::subscriber::with_default(subscriber, || {
            request_span(request)
                .context()
                .span()
                .span_context()
                .clone()
        })
    }

    /// A request without a `traceparent` starts a sampled trace of its own instead of
    /// inheriting the current context's unsampled flag.
    #[test]
    fn starts_a_sampled_trace_without_a_traceparent() -> eyre::Result<()> {
        let request = Request::get("/cors").body(Body::empty())?;
        let span_context = traced_span_context(&request);
        assert!(span_context.is_valid());
        assert!(span_context.is_sampled());
        Ok(())
    }

    #[test]
    fn continues_an_incoming_trace() -> eyre::Result<()> {
        let request = Request::get("/cors")
            .header(
                "traceparent",
                "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            )
            .body(Body::empty())?;
        let span_context = traced_span_context(&request);
        assert_eq!(
            span_context.trace_id(),
            TraceId::from_hex("0af7651916cd43dd8448eb211c80319c")?
        );
        assert!(span_context.is_sampled());
        Ok(())
    }
}
