//! The proxy's metrics, as `tracing` metric events.
//!
//! Every field of a metric event becomes a label of the metric, its message included, so these
//! events have no message and are labelled only with an allowlist rule or [`OTHER_SERVER`],
//! which keeps each metric to a handful of series.
//!
//! - `tcp_over_ws_proxy_connections`: connection requests, by `outcome` (`connected`, `failed`,
//!   `refused` or `invalid`), with an `error.type` of `connect` or `timeout` when one failed.
//! - `tcp_over_ws_proxy_active_relays`: the relays open right now.
//! - `tcp_over_ws_proxy_relay_duration_seconds`: how long each relay stayed open.
//! - `tcp_over_ws_proxy_bytes`: the bytes relayed, by `direction`.

use std::time::{Duration, Instant};

use crate::target::AllowedTcpTarget;

/// The `server.address` of a request whose target is not on the allowlist, however many
/// different hosts clients ask for.
const OTHER_SERVER: &str = "other";

/// How long relayed bytes may wait before they are counted, so a stream that runs for hours
/// shows its traffic while it runs and still logs only one line per minute and direction.
const BYTES_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// Why a request was turned away before any connection was attempted.
#[derive(Clone, Copy)]
pub(crate) enum Unrelayed {
    /// The target is not on the allowlist.
    Refused,
    /// The target could not be parsed.
    Invalid,
}

/// Why a connection to an allowed target could not be made.
#[derive(Clone, Copy)]
pub(crate) enum ConnectFailure {
    /// The target refused or could not be reached.
    Connect,
    /// The target did not answer within the connect timeout.
    Timeout,
}

/// Which way relayed bytes went.
#[derive(Clone, Copy)]
pub(crate) enum Direction {
    ToTcp,
    ToWebSocket,
}

/// Counts a request that was turned away before any connection was attempted.
pub(crate) fn count_unrelayed(reason: Unrelayed) {
    let outcome = match reason {
        Unrelayed::Refused => "refused",
        Unrelayed::Invalid => "invalid",
    };
    tracing::info!(
        monotonic_counter.tcp_over_ws_proxy_connections = 1_u64,
        outcome,
        server.address = OTHER_SERVER,
    );
}

/// Counts a connection to `target` that could not be made.
pub(crate) fn count_failed(target: &AllowedTcpTarget, failure: ConnectFailure) {
    let error_type = match failure {
        ConnectFailure::Connect => "connect",
        ConnectFailure::Timeout => "timeout",
    };
    tracing::info!(
        monotonic_counter.tcp_over_ws_proxy_connections = 1_u64,
        outcome = "failed",
        error.type = error_type,
        server.address = target.host(),
        server.port = target.port(),
    );
}

/// A connected relay, counted among the active ones until it drops, however the relay ends.
pub(crate) struct ActiveRelay<'a> {
    target: &'a AllowedTcpTarget,
    opened: Instant,
}

impl<'a> ActiveRelay<'a> {
    /// Counts the connection to `target` and the relay as active.
    pub(crate) fn open(target: &'a AllowedTcpTarget) -> Self {
        tracing::info!(
            monotonic_counter.tcp_over_ws_proxy_connections = 1_u64,
            outcome = "connected",
            server.address = target.host(),
            server.port = target.port(),
        );
        // Its own event, since the decrement must carry exactly the same labels.
        tracing::info!(
            counter.tcp_over_ws_proxy_active_relays = 1_i64,
            server.address = target.host(),
            server.port = target.port(),
        );
        Self {
            target,
            opened: Instant::now(),
        }
    }
}

impl Drop for ActiveRelay<'_> {
    fn drop(&mut self) {
        tracing::info!(
            counter.tcp_over_ws_proxy_active_relays = -1_i64,
            histogram.tcp_over_ws_proxy_relay_duration_seconds =
                self.opened.elapsed().as_secs_f64(),
            server.address = self.target.host(),
            server.port = self.target.port(),
        );
    }
}

/// The bytes relayed in one direction, counted at most once per [`BYTES_FLUSH_INTERVAL`] and
/// once more when it drops.
pub(crate) struct RelayedBytes<'a> {
    target: &'a AllowedTcpTarget,
    direction: Direction,
    total: u64,
    uncounted: u64,
    last_counted: Instant,
}

impl<'a> RelayedBytes<'a> {
    pub(crate) fn new(target: &'a AllowedTcpTarget, direction: Direction) -> Self {
        Self {
            target,
            direction,
            total: 0,
            uncounted: 0,
            last_counted: Instant::now(),
        }
    }

    pub(crate) fn add(&mut self, bytes: usize) {
        let bytes = bytes as u64;
        self.total += bytes;
        self.uncounted += bytes;
        if self.last_counted.elapsed() >= BYTES_FLUSH_INTERVAL {
            self.count();
        }
    }

    /// All bytes relayed in this direction so far.
    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    fn count(&mut self) {
        if self.uncounted > 0 {
            let direction = match self.direction {
                Direction::ToTcp => "to_tcp",
                Direction::ToWebSocket => "to_websocket",
            };
            tracing::info!(
                monotonic_counter.tcp_over_ws_proxy_bytes = self.uncounted,
                direction,
                server.address = self.target.host(),
                server.port = self.target.port(),
            );
            self.uncounted = 0;
        }
        self.last_counted = Instant::now();
    }
}

impl Drop for RelayedBytes<'_> {
    fn drop(&mut self) {
        self.count();
    }
}
