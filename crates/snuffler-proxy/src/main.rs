#[tokio::main]
async fn main() -> snuffler_proxy::Error {
    snuffler_proxy::run_from_env().await
}
