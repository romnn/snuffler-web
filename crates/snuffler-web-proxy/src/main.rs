//! Local snuffler-web proxy process entry point.

use color_eyre::eyre;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    snuffler_web_proxy::run_from_env().await?;
    Ok(())
}
