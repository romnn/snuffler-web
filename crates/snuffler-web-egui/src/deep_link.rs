//! `?example=<slug>` — a link that opens the app on a dataset.
//!
//! Example data exists so that someone with none of their own can still learn the
//! tool. A link is how that reaches them: a lecturer sends one URL and the class
//! arrives looking at the same waveforms, with no download, no unzip, and no
//! instructions about which menu to use.
//!
//! Two directions, both cheap. On boot the slug is read out of the query string
//! and handed to the bridge, which resolves it against the example index. And when
//! an example is loaded from the menu, the address bar is rewritten to the link
//! that reproduces it — so the way to share what you are looking at is to copy the
//! URL, which is where everyone looks first.

/// The example slug asked for by the URL, if any.
pub fn requested_example() -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix("example=").map(str::to_owned))
        .map(|slug| decode(&slug))
        .filter(|slug| !slug.is_empty())
}

/// Point the address bar at the example now loaded, without touching anything else
/// in the query string or adding a history entry.
///
/// `replaceState` rather than `pushState`: loading a dataset is not navigation, and
/// making the back button undo a load would be a lie about what it does.
pub fn remember_example(slug: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(url) = web_sys::Url::new(&window.location().href().unwrap_or_default()) else {
        return;
    };
    url.search_params().set("example", slug);
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url.href()));
    }
}

/// Percent-decoding, for the one character a slug can realistically carry (`%2E`
/// for the dot in a magnitude). A slug that arrives malformed simply fails to
/// match an example, which the bridge reports by name.
fn decode(value: &str) -> String {
    js_sys::decode_uri_component(value)
        .ok()
        .and_then(|decoded| decoded.as_string())
        .unwrap_or_else(|| value.to_owned())
}
