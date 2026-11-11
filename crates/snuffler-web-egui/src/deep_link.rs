//! `?example=<slug>` — a link that opens the app on a dataset.
//!
//! Example data exists so that someone with none of their own can still learn the tool.
//! A link is how that reaches them: a lecturer sends one URL and the class arrives looking at
//! the same waveforms, with no download, no unzip, and no instructions about which menu to
//! use.
//!
//! Two directions, both cheap.
//! On boot the slug is read out of the query string and handed to the bridge, which resolves
//! it against the example index.
//! And when an example is loaded from the menu, the address bar is rewritten to the link that
//! reproduces it — so the way to share what you are looking at is to copy the URL, which is
//! where everyone looks first.

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

/// A `?url=` waveform to open on boot — the browser twin of snuffler's
/// positional path arguments — with the optional `?format=` override beside it,
/// the twin of `--format` (pyrocko's sniffer knows ten of its seventeen
/// loadable formats; segy, seisan, kan, gcf and css need naming).
pub fn requested_url() -> Option<(String, String)> {
    let search = web_sys::window()?.location().search().ok()?;
    let pairs: Vec<&str> = search.trim_start_matches('?').split('&').collect();
    let url = pairs
        .iter()
        .find_map(|pair| pair.strip_prefix("url="))
        .map(decode)
        .filter(|url| !url.is_empty())?;
    let format = pairs
        .iter()
        .find_map(|pair| pair.strip_prefix("format="))
        .map(decode)
        .filter(|format| !format.is_empty())
        .unwrap_or_else(|| "detect".to_owned());
    Some((url, format))
}

/// Whether `?demo=1` asked for the synthetic demo instead of a recorded dataset.
///
/// The demo is not in the File menu, because real recordings teach better.
/// It stays reachable here because the parity harness needs it: tiers 2 and 3
/// measure the web against a Qt viewer holding the *same* synthetic pile, generated
/// by the same code with the same seed, and no recorded dataset can stand in for that.
pub fn wants_demo() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    window.location().search().is_ok_and(|search| {
        search
            .trim_start_matches('?')
            .split('&')
            .any(|pair| matches!(pair, "demo=1" | "demo=true"))
    })
}

/// Point the address bar at the example now loaded, without touching anything else
/// in the query string or adding a history entry.
///
/// `replaceState` rather than `pushState`: loading a dataset is not navigation, and
/// making the back button undo a load would be a lie about what it does.
///
/// The competing startup parameters go: booting prefers `url`/`format` over
/// `example` and `example` over `demo`, so leaving any of them behind would make
/// the rewritten address reopen the *old* dataset when copied or reloaded.
pub fn remember_example(slug: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(url) = web_sys::Url::new(&window.location().href().unwrap_or_default()) else {
        return;
    };
    for stale in ["url", "format", "demo"] {
        url.search_params().delete(stale);
    }
    url.search_params().set("example", slug);
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url.href()));
    }
}

/// Percent-decoding, for the one character a slug can realistically carry (`%2E` for the dot
/// in a magnitude).
///
/// A slug that arrives malformed simply fails to match an example, which the bridge reports
/// by name.
fn decode(value: &str) -> String {
    js_sys::decode_uri_component(value)
        .ok()
        .and_then(|decoded| decoded.as_string())
        .unwrap_or_else(|| value.to_owned())
}
