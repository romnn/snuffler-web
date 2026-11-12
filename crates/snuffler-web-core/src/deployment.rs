//! Browser persistence scoped to the directory containing one deployed application.

/// Qualifies a storage key with the deployment's base URL pathname.
///
/// Builds at nested paths share a browser origin, but keep independent settings and history.
#[must_use]
pub fn storage_key(base_path: &str, name: &str) -> String {
    format!("snuffler:{base_path}:{name}")
}

/// Resolves a storage key against the document's configured deployment base.
///
/// An unavailable document or invalid base disables persistence rather than sharing
/// a fallback key.
#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn browser_storage_key(name: &str) -> Option<String> {
    let base = web_sys::window()?.document()?.base_uri().ok()??;
    let url = web_sys::Url::new(&base).ok()?;
    Some(storage_key(&url.pathname(), name))
}

#[cfg(test)]
mod tests {
    use super::storage_key;

    #[test]
    fn nested_deployments_have_independent_settings_and_history() {
        for name in ["settings.wiggle_theme", "command_history"] {
            assert_ne!(
                storage_key("/snuffler-web/", name),
                storage_key("/snuffler-web/latest/", name)
            );
        }
        assert_eq!(
            storage_key("/snuffler-web/latest/", "settings.wiggle_theme"),
            "snuffler:/snuffler-web/latest/:settings.wiggle_theme"
        );
    }
}
