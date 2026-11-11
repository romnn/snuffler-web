//! snuffler-web's local proxy: the generic CORS and TCP-over-WebSocket proxies, allowed to
//! reach the hosts the app talks to.
//!
//! Routes:
//!
//! - `/health`: liveness probe.
//! - `/cors?url=…`: FDSN requests, through [`cors_proxy`].
//! - `/tcp?host=…&port=…`: the live `SeedLink` stream, through [`tcp_over_ws_proxy`].
//!
//! The built-in hosts are in [`defaults`].
//! A deployment allows more with the `--allow-*` flags or their environment variables, which a
//! `.env` file in the working directory may also set.

mod defaults;

use std::future::pending;
use std::net::SocketAddr;
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

/// Why the proxy could not start, or stopped serving.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The `.env` file in the working directory exists but could not be read.
    #[error("failed to load the .env file")]
    EnvFile(#[source] dotenvy::Error),
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

/// Loads `.env`, parses the command line and runs the local proxy until shutdown.
///
/// Variables already set in the environment win over the file.
///
/// # Errors
///
/// Returns [`RunError::EnvFile`] for an unreadable `.env`, and fails like [`run`] otherwise.
pub async fn run_from_env() -> Result<(), RunError> {
    init_tracing();
    if let Some(path) = load_env_file()? {
        info!(path = %path.display(), "loaded environment file");
    }
    run(Args::parse()).await
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
        .pool_max_idle_per_host(32)
        .build()
        .map_err(RunError::HttpClient)?;

    let state = Arc::new(ProxyState {
        cors: cors_proxy::Proxy::new(http_client, http_allowlist),
        tcp: tcp_over_ws_proxy::Proxy::new(connect_timeout, tcp_allowlist),
    });
    let app = Router::new()
        .route("/health", get(health))
        .route("/cors", any(handle_cors))
        .route("/tcp", get(handle_tcp))
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
