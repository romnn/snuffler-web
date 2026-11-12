//! Build identity for deployed bundles and local development checkouts.

use std::env;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo::rerun-if-env-changed=SNUFFLER_GIT_COMMIT");
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src");
    println!("cargo::rerun-if-changed=Cargo.toml");
    println!("cargo::rerun-if-changed=../../Cargo.toml");
    println!("cargo::rerun-if-changed=../../Cargo.lock");
    // Follow both loose and packed refs, including worktree-specific HEAD files.
    for name in ["HEAD".to_owned(), "packed-refs".to_owned()]
        .into_iter()
        .chain(git(&["symbolic-ref", "-q", "HEAD"]))
    {
        if let Some(path) = git(&["rev-parse", "--git-path", &name]) {
            println!("cargo::rerun-if-changed={path}");
        }
    }
    let commit = env::var("SNUFFLER_GIT_COMMIT")
        .ok()
        .filter(|commit| !commit.is_empty())
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo::rustc-env=SNUFFLER_BUILD_COMMIT={commit}");
    if let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) {
        println!(
            "cargo::rustc-env=SNUFFLER_LOCAL_BUILD_EPOCH={}",
            elapsed.as_secs()
        );
    }
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
