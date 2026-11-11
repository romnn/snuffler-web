//! The proxy's metrics, as `tracing` metric events.
//!
//! Every field of a metric event becomes a label of the metric, its message included, so these
//! events have no message and are labelled only with an allowlisted upstream host or
//! [`OTHER_SERVER`], which keeps each metric to a handful of series.
//!
//! - `cors_proxy_requests`: every request but a preflight, by `outcome` (see [`Outcome`]),
//!   upstream `server.address` and response status, with an `error.type` when the upstream
//!   request failed.
//! - `cors_proxy_upstream_duration_ms`: how long the upstream took to answer, or to fail, for
//!   the requests that reached it.
//! - `cors_proxy_pending_upstream_requests`: requests waiting on their upstream right now, which
//!   climbs when an upstream hangs.

use std::time::Duration;

use axum::http::StatusCode;

/// The `server.address` of a request whose target is not on the allowlist, however many
/// different hosts clients ask for.
pub(crate) const OTHER_SERVER: &str = "other";

/// How a request ended, as the `outcome` label of `cors_proxy_requests`.
#[derive(Clone, Copy)]
pub(crate) enum Outcome {
    /// The upstream answered, with whatever status.
    Forwarded,
    /// The target is not on the allowlist.
    Denied,
    /// The method or the target URL is unusable.
    Invalid,
    /// The upstream could not be reached, or its response not passed on.
    UpstreamFailed,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Forwarded => "forwarded",
            Self::Denied => "denied",
            Self::Invalid => "invalid",
            Self::UpstreamFailed => "upstream_failed",
        }
    }

    fn reached_upstream(self) -> bool {
        matches!(self, Self::Forwarded | Self::UpstreamFailed)
    }
}

/// One handled request, as its metrics see it.
pub(crate) struct HandledRequest<'a> {
    pub(crate) outcome: Outcome,
    /// Why the upstream request failed, for [`Outcome::UpstreamFailed`].
    pub(crate) upstream_error: Option<&'static str>,
    /// The allowlisted upstream host, or [`OTHER_SERVER`].
    pub(crate) server_address: &'a str,
    /// The status the proxy answered with.
    pub(crate) status: StatusCode,
    /// From the request's arrival to the upstream's response headers, or its failure.
    pub(crate) elapsed: Duration,
}

/// A request waiting on its upstream, counted among the pending ones until it drops, whether
/// the upstream answered, failed or the client went away.
pub(crate) struct PendingUpstream<'a> {
    server_address: &'a str,
}

impl<'a> PendingUpstream<'a> {
    pub(crate) fn start(server_address: &'a str) -> Self {
        tracing::info!(
            counter.cors_proxy_pending_upstream_requests = 1_i64,
            server.address = server_address,
        );
        Self { server_address }
    }
}

impl Drop for PendingUpstream<'_> {
    fn drop(&mut self) {
        tracing::info!(
            counter.cors_proxy_pending_upstream_requests = -1_i64,
            server.address = self.server_address,
        );
    }
}

pub(crate) fn count_request(request: &HandledRequest<'_>) {
    let upstream_duration_ms = request
        .outcome
        .reached_upstream()
        .then_some(request.elapsed.as_secs_f64() * 1000.0);
    tracing::info!(
        monotonic_counter.cors_proxy_requests = 1_u64,
        histogram.cors_proxy_upstream_duration_ms = upstream_duration_ms,
        outcome = request.outcome.as_str(),
        error.type = request.upstream_error,
        server.address = request.server_address,
        http.response.status_code = request.status.as_u16(),
    );
}
