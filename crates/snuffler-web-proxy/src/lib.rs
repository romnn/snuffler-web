//! Local allowlisted proxy combining the HTTP CORS and `SeedLink` endpoints.
//!
//! Routes:
//!
//! - `/health`: liveness probe.
//! - `/cors?url=…`: FDSN requests, through [`snuffler_web_cors_proxy`].
//! - `/seedlink?host=…&port=…`: live streams, through [`snuffler_web_tcp_ws_proxy`].

use std::future::pending;
use std::net::SocketAddr;
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
use tracing::{info, warn};

const DEFAULT_BIND: &str = "127.0.0.1:17684";
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;

/// Command-line configuration for the local proxy process.
#[derive(Clone, Debug, Parser)]
#[command(about = "snuffler-web local HTTP CORS and WebSocket TCP proxy")]
pub struct Args {
    /// HTTP listen address for both proxy endpoints.
    #[arg(long, env = "SNUFFLER_PROXY_BIND", default_value = DEFAULT_BIND)]
    pub bind: SocketAddr,

    /// Timeout for outgoing HTTP and SeedLink/ringserver TCP connections.
    #[arg(
        long,
        env = "SNUFFLER_PROXY_CONNECT_TIMEOUT_MS",
        default_value_t = DEFAULT_CONNECT_TIMEOUT_MS
    )]
    pub connect_timeout_ms: u64,

    /// Extra allowed HTTP target URLs, including exact path.
    ///
    /// Defaults already include snuffler-web's known FDSN dataselect, event and
    /// station query endpoints.
    /// Repeat this flag, or set a comma-separated
    /// `SNUFFLER_PROXY_ALLOW_HTTP_TARGETS`, to add deployment-specific endpoints.
    #[arg(
        long = "allow-http-target",
        env = "SNUFFLER_PROXY_ALLOW_HTTP_TARGETS",
        value_delimiter = ','
    )]
    pub allow_http_targets: Vec<String>,

    /// Extra allowed TCP targets in `host:port` form.
    ///
    /// Defaults already include the SeedLink/ringserver targets used by local
    /// snuffler-web development.
    /// Repeat this flag, or set a comma-separated
    /// `SNUFFLER_PROXY_ALLOW_TCP_TARGETS`, to add deployment-specific streams.
    #[arg(
        long = "allow-tcp-target",
        env = "SNUFFLER_PROXY_ALLOW_TCP_TARGETS",
        value_delimiter = ','
    )]
    pub allow_tcp_targets: Vec<String>,
}

/// Why the proxy could not start, or stopped serving.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
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
        source: snuffler_web_cors_proxy::TargetError,
    },
    /// An `--allow-tcp-target` value is not a usable rule.
    #[error("invalid allowed TCP target {target:?}")]
    TcpTarget {
        /// The rejected value.
        target: String,
        /// Why it was rejected.
        #[source]
        source: snuffler_web_tcp_ws_proxy::TargetError,
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
    cors: snuffler_web_cors_proxy::Proxy,
    seedlink: snuffler_web_tcp_ws_proxy::Proxy,
}

/// Parses command-line arguments and runs the local proxy until shutdown.
///
/// # Errors
///
/// Fails like [`run`].
pub async fn run_from_env() -> Result<(), RunError> {
    let args = Args::parse();
    run(args).await
}

/// Runs the local proxy with explicit configuration until shutdown.
///
/// # Errors
///
/// Returns a [`RunError`] variant naming the step that failed: client setup, an allowlist
/// entry, binding the listen address, or serving.
pub async fn run(args: Args) -> Result<(), RunError> {
    init_tracing();

    let connect_timeout = Duration::from_millis(args.connect_timeout_ms);
    let http_client = reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(32)
        .build()
        .map_err(RunError::HttpClient)?;

    let state = Arc::new(ProxyState {
        cors: snuffler_web_cors_proxy::Proxy::new(
            http_client,
            http_allowlist(&args.allow_http_targets)?,
        ),
        seedlink: snuffler_web_tcp_ws_proxy::Proxy::new(
            connect_timeout,
            tcp_allowlist(&args.allow_tcp_targets)?,
        ),
    });
    let app = Router::new()
        .route("/health", get(health))
        .route("/cors", any(handle_cors))
        .route("/seedlink", get(handle_seedlink))
        .with_state(state);

    let listener = TcpListener::bind(args.bind)
        .await
        .map_err(|source| RunError::Bind {
            address: args.bind,
            source,
        })?;
    info!("snuffler proxy listening on http://{}", args.bind);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(RunError::Serve)
}

/// Logs at `info` for this crate unless `RUST_LOG` says otherwise.
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "snuffler_web_proxy=info,tower_http=warn".into()),
        )
        .init();
}

/// The built-in FDSN endpoints plus every `--allow-http-target`.
fn http_allowlist(
    extra_targets: &[String],
) -> Result<snuffler_web_cors_proxy::Allowlist, RunError> {
    let extra_rules = extra_targets
        .iter()
        .map(|target| {
            snuffler_web_cors_proxy::AllowedHttpTarget::parse(target).map_err(|source| {
                RunError::HttpTarget {
                    target: target.clone(),
                    source,
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(snuffler_web_cors_proxy::Allowlist::with_default_rules(
        extra_rules,
    ))
}

/// The built-in `SeedLink` servers plus every `--allow-tcp-target`.
fn tcp_allowlist(
    extra_targets: &[String],
) -> Result<snuffler_web_tcp_ws_proxy::Allowlist, RunError> {
    let extra_rules = extra_targets
        .iter()
        .map(|target| {
            snuffler_web_tcp_ws_proxy::AllowedTcpTarget::parse(target).map_err(|source| {
                RunError::TcpTarget {
                    target: target.clone(),
                    source,
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(snuffler_web_tcp_ws_proxy::Allowlist::with_default_targets(
        extra_rules,
    ))
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

async fn handle_seedlink(
    State(state): State<Arc<ProxyState>>,
    upgrade: WebSocketUpgrade,
    request: Request<Body>,
) -> Response<Body> {
    state.seedlink.handle(upgrade, &request)
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
