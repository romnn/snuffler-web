//! snuffler-web's local proxy: the generic CORS and TCP-over-WebSocket proxies, allowed to
//! reach the hosts the app talks to.
//!
//! Routes:
//!
//! - `/health`: liveness probe.
//! - `/cors?url=…`: FDSN requests, through [`cors_proxy`].
//! - `/tcp?host=…&port=…`: the live `SeedLink` stream, through [`tcp_over_ws_proxy`].
//!
//! The two proxy routes get a tower-http [`TraceLayer`] span and log line per request (see
//! [`http_trace`]), and count their own metrics.
//! `/health` gets neither, since a container healthcheck polls it every few seconds.
//!
//! The built-in hosts are in [`defaults`].
//! A deployment allows more with the `--allow-*` flags or their environment variables, which a
//! `.env` file in the working directory may also set.

mod defaults;
mod http_trace;
pub mod telemetry;

use std::future::pending;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::{Request, Response};
use axum::routing::{any, get};
use clap::Parser;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

const DEFAULT_BIND: &str = "127.0.0.1:9000";
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;

/// Command-line configuration for the local proxy process.
#[derive(Clone, Debug, Parser)]
#[command(about = "snuffler-web local HTTP CORS and WebSocket TCP proxy")]
pub struct Args {
    /// Checks the running proxy's local /health endpoint and exits without starting a server.
    #[arg(long)]
    pub healthcheck: bool,

    /// HTTP listen address for both proxy endpoints.
    #[arg(long, env = "SNUFFLER_PROXY_BIND", default_value = DEFAULT_BIND)]
    pub bind: SocketAddr,

    /// Timeout for outgoing HTTP and TCP connections.
    #[arg(
        long,
        env = "SNUFFLER_PROXY_CONNECT_TIMEOUT_MS",
        default_value_t = DEFAULT_CONNECT_TIMEOUT_MS
    )]
    pub connect_timeout_ms: u64,

    /// More HTTP target URLs to allow, each matched by scheme, host, port and exact path.
    ///
    /// The app's FDSN query endpoints are always allowed.
    /// Repeat the flag, or set a comma-separated `SNUFFLER_PROXY_ALLOW_HTTP_TARGETS`.
    #[arg(
        long = "allow-http-target",
        env = "SNUFFLER_PROXY_ALLOW_HTTP_TARGETS",
        value_delimiter = ','
    )]
    pub allow_http_targets: Vec<String>,

    /// How to write the stderr log: `pretty` for a terminal, `json` for a log shipper.
    #[arg(long, env = "SNUFFLER_PROXY_LOG_FORMAT", value_enum, default_value_t)]
    pub log_format: telemetry::LogFormat,

    /// Which events to log and export, as `RUST_LOG`-style directives such as `debug` or
    /// `info,cors_proxy=trace`.
    ///
    /// Defaults to `info` for the proxy's own crates.
    /// A filter that does not parse stops the proxy at startup.
    #[arg(long, env = "RUST_LOG")]
    pub log_filter: Option<String>,

    /// OpenTelemetry collector to export traces, metrics and logs to over OTLP gRPC, such as
    /// `http://localhost:4317`.
    ///
    /// Without it, the proxy only logs to stderr.
    #[arg(long, env = "OTEL_EXPORTER_OTLP_ENDPOINT")]
    pub otel_endpoint: Option<String>,

    /// Whether log events are exported with the traces and metrics: `otlp`, or `none` to keep
    /// them to stderr, such as where a log shipper already collects it.
    #[arg(long, env = "OTEL_LOGS_EXPORTER", value_enum, default_value_t)]
    pub otel_logs_exporter: telemetry::LogsExporter,

    /// More TCP targets to allow, in `host:port` form.
    ///
    /// The app's `SeedLink` servers are always allowed.
    /// Repeat the flag, or set a comma-separated `SNUFFLER_PROXY_ALLOW_TCP_TARGETS`.
    #[arg(
        long = "allow-tcp-target",
        env = "SNUFFLER_PROXY_ALLOW_TCP_TARGETS",
        value_delimiter = ','
    )]
    pub allow_tcp_targets: Vec<String>,
}

/// Why the proxy could not start, stopped serving, or failed its healthcheck.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The local health endpoint could not be reached within two seconds.
    #[error("proxy healthcheck request failed")]
    Healthcheck(#[source] reqwest::Error),
    /// The local health endpoint returned a status other than 200.
    #[error("proxy healthcheck returned {0}")]
    HealthStatus(reqwest::StatusCode),
    /// The `.env` file in the working directory exists but could not be read.
    #[error("failed to load the .env file")]
    EnvFile(#[source] dotenvy::Error),
    /// Telemetry could not be set up.
    #[error(transparent)]
    Telemetry(#[from] telemetry::TelemetryError),
    /// The upstream HTTP client could not be configured.
    #[error("failed to build the upstream HTTP client")]
    HttpClient(#[source] reqwest::Error),
    /// An `--allow-http-target` value is not a usable rule.
    #[error("invalid allowed HTTP target {target:?}")]
    HttpTarget {
        /// The rejected value.
        target: String,
        /// Why it was rejected.
        #[source]
        source: cors_proxy::TargetError,
    },
    /// An `--allow-tcp-target` value is not a usable rule.
    #[error("invalid allowed TCP target {target:?}")]
    TcpTarget {
        /// The rejected value.
        target: String,
        /// Why it was rejected.
        #[source]
        source: tcp_over_ws_proxy::TargetError,
    },
    /// The listen address could not be bound.
    #[error("failed to bind {address}")]
    Bind {
        /// The `--bind` address.
        address: SocketAddr,
        /// Why binding failed.
        #[source]
        source: std::io::Error,
    },
    /// The server stopped with an I/O error.
    #[error("proxy server failed")]
    Serve(#[source] std::io::Error),
}

/// The two proxies the routes share.
#[derive(Clone)]
struct ProxyState {
    cors: cors_proxy::Proxy,
    tcp: tcp_over_ws_proxy::Proxy,
}

/// Loads configuration and runs the proxy or checks its health.
///
/// Variables already set in the environment win over the file.
/// `--healthcheck` probes the listen address and exits without telemetry.
/// Wildcard listen addresses are reached through loopback.
///
/// # Errors
///
/// Returns [`RunError::EnvFile`] for an unreadable `.env`, [`RunError::Telemetry`] when the
/// OTLP exporters cannot be built, [`RunError::Healthcheck`] or [`RunError::HealthStatus`]
/// for a failed health probe, and fails like [`run`] otherwise.
pub async fn run_from_env() -> Result<(), RunError> {
    let env_file = load_env_file()?;
    let args = Args::parse();
    if args.healthcheck {
        return check_health(args.bind).await;
    }
    // Flushes the exported telemetry when the proxy stops.
    let _telemetry = telemetry::init(
        args.log_format,
        args.log_filter
            .as_deref()
            .filter(|filter| !filter.trim().is_empty()),
        args.otel_endpoint.as_deref(),
        args.otel_logs_exporter,
    )?;
    if let Some(path) = env_file {
        info!(path = %path.display(), "loaded environment file");
    }
    run(args).await
}

async fn check_health(mut address: SocketAddr) -> Result<(), RunError> {
    // An unspecified listen address must be reached through its matching loopback family.
    if address.ip().is_unspecified() {
        address.set_ip(match address.ip() {
            IpAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
            IpAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
        });
    }
    let response = reqwest::Client::builder()
        // A liveness probe must never use a deployment's outbound proxy or follow redirects.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(RunError::Healthcheck)?
        .get(format!("http://{address}/health"))
        .send()
        .await
        .map_err(RunError::Healthcheck)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(RunError::HealthStatus(response.status()));
    }
    Ok(())
}

/// Runs the local proxy with explicit configuration until shutdown.
///
/// # Errors
///
/// Returns a [`RunError`] variant naming the step that failed: client setup, an allowlist
/// entry, binding the listen address, or serving.
pub async fn run(args: Args) -> Result<(), RunError> {
    let http_allowlist = http_allowlist(&args.allow_http_targets)?;
    let tcp_allowlist = tcp_allowlist(&args.allow_tcp_targets)?;

    let connect_timeout = Duration::from_millis(args.connect_timeout_ms);
    let http_client = reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(32);
    let cors = cors_proxy::Proxy::new(http_client, http_allowlist).map_err(RunError::HttpClient)?;

    let state = Arc::new(ProxyState {
        cors,
        tcp: tcp_over_ws_proxy::Proxy::new(connect_timeout, tcp_allowlist),
    });
    // A layer wraps only the routes added before it, which keeps `/health` out.
    let app = Router::new()
        .route("/cors", any(handle_cors))
        .route("/tcp", get(handle_tcp))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(http_trace::request_span)
                .on_request(http_trace::record_trace_id)
                .on_response(http_trace::record_response)
                // A 5xx is already marked on the span and logged by the handler that failed.
                .on_failure(()),
        )
        .route("/health", get(health))
        .with_state(state);

    let listener = TcpListener::bind(args.bind)
        .await
        .map_err(|source| RunError::Bind {
            address: args.bind,
            source,
        })?;
    info!("snuffler proxy listening on http://{}", args.bind);
    // The peer address is what the request span records as `client.address`.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(RunError::Serve)
}

/// Loads `.env` from the working directory, if there is one.
///
/// Only that one file: `dotenvy::dotenv` would also search the parent directories and could
/// pick up an unrelated `.env`, such as one in the user's home directory.
fn load_env_file() -> Result<Option<PathBuf>, RunError> {
    let path = PathBuf::from(".env");
    match dotenvy::from_path(&path) {
        Ok(()) => Ok(Some(path)),
        Err(err) if err.not_found() => Ok(None),
        Err(err) => Err(RunError::EnvFile(err)),
    }
}

/// The configured targets, trimmed, without the blanks a trailing comma leaves.
fn configured_targets(values: &[String]) -> impl Iterator<Item = String> {
    values
        .iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The app's FDSN endpoints plus every configured HTTP target.
fn http_allowlist(targets: &[String]) -> Result<cors_proxy::Allowlist, RunError> {
    let rules = defaults::fdsn_query_urls()
        .chain(configured_targets(targets))
        .map(|target| {
            cors_proxy::AllowedHttpTarget::parse(&target)
                .map_err(|source| RunError::HttpTarget { target, source })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(cors_proxy::Allowlist::new(rules))
}

/// The app's `SeedLink` servers plus every configured TCP target.
fn tcp_allowlist(targets: &[String]) -> Result<tcp_over_ws_proxy::Allowlist, RunError> {
    let rules = defaults::SEEDLINK_SERVERS
        .iter()
        .map(|&target| target.to_owned())
        .chain(configured_targets(targets))
        .map(|target| {
            tcp_over_ws_proxy::AllowedTcpTarget::parse(&target)
                .map_err(|source| RunError::TcpTarget { target, source })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(tcp_over_ws_proxy::Allowlist::new(rules))
}

async fn health() -> &'static str {
    "ok\n"
}

async fn handle_cors(
    State(state): State<Arc<ProxyState>>,
    request: Request<Body>,
) -> Response<Body> {
    state.cors.handle(request).await
}

async fn handle_tcp(
    State(state): State<Arc<ProxyState>>,
    upgrade: WebSocketUpgrade,
    request: Request<Body>,
) -> Response<Body> {
    state.tcp.handle(upgrade, &request)
}

/// Resolves on Ctrl-C, or on SIGTERM where there is one.
///
/// A handler that cannot be installed never resolves, leaving the other one in charge.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            warn!(%err, "failed to install ctrl-c handler");
            pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                warn!(%err, "failed to install terminate handler");
                pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    use super::{RunError, check_health};

    #[tokio::test]
    async fn healthcheck_accepts_only_ok() -> eyre::Result<()> {
        for status in ["200 OK", "302 Found", "503 Service Unavailable"] {
            let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
            // Use the wildcard bind address to exercise loopback normalization as in Docker.
            let address = listener.local_addr()?;
            let server = async {
                let (stream, _) = listener.accept().await?;
                let mut stream = BufReader::new(stream);
                loop {
                    let mut line = String::new();
                    eyre::ensure!(
                        stream.read_line(&mut line).await? > 0,
                        "request ended early"
                    );
                    if line == "\r\n" {
                        break;
                    }
                }
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nLocation: /health\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await?;
                Ok::<(), eyre::Report>(())
            };
            let (result, response_result) =
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    tokio::join!(check_health(address), server)
                })
                .await?;
            response_result?;
            // Redirects and server errors must fail the probe instead of being followed.
            if status == "200 OK" {
                result?;
            } else {
                assert!(
                    matches!(result, Err(RunError::HealthStatus(_))),
                    "{result:?}"
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn healthcheck_rejects_a_closed_port() -> eyre::Result<()> {
        let address = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            listener.local_addr()?
        };
        let result = check_health(address).await;
        assert!(
            matches!(result, Err(RunError::Healthcheck(_))),
            "{result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn healthcheck_times_out_without_a_response() -> eyre::Result<()> {
        // Keep the listener open without serving HTTP so TCP connects but HTTP stalls.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            check_health(listener.local_addr()?),
        )
        .await?;
        assert!(matches!(result, Err(RunError::Healthcheck(err)) if err.is_timeout()));
        Ok(())
    }
}
