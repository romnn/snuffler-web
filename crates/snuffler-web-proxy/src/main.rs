//! Local snuffler-web proxy process entry point.

#[tokio::main]
async fn main() -> snuffler_web_proxy::Error {
    snuffler_web_proxy::run_from_env().await
}
