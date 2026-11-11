//! Where snuffler-web-proxy is, for live `SeedLink` streams.
//!
//! A deployment names its proxy in `snufflerConfig.proxyUrl`, which the page's `config.js` sets
//! before the app starts (`web/config.js` documents it).
//! Without one the app cannot stream live `SeedLink` data.
//! The Python worker reads the same file for its FDSN requests (`web/snuffler_bridge/fdsn.py`).

use wasm_bindgen::JsValue;

use super::js_value_to_string;

thread_local! {
    static BASE: std::cell::OnceCell<Option<String>> = const { std::cell::OnceCell::new() };
}

/// The configured proxy, resolved against the page and ending in `/`.
///
/// The configuration is read once, on the first call.
/// An unusable value is logged and ignored, so the app keeps working without the proxy.
pub(super) fn base() -> Option<String> {
    BASE.with(|base| base.get_or_init(read_base).clone())
}

fn read_base() -> Option<String> {
    let window = web_sys::window()?;
    let config = js_sys::Reflect::get(&window, &"snufflerConfig".into()).ok()?;
    // `Reflect.get` throws on an undefined config, which `ok()` turns into "not configured".
    let proxy = js_sys::Reflect::get(&config, &"proxyUrl".into())
        .ok()?
        .as_string()?;
    let proxy = proxy.trim();
    if proxy.is_empty() {
        return None;
    }
    let page = window.document()?.base_uri().ok()??;
    let url = match web_sys::Url::new_with_base(proxy, &page) {
        Ok(url) if matches!(url.protocol().as_str(), "http:" | "https:") => url,
        Ok(url) => {
            tracing::warn!(
                proxy,
                protocol = url.protocol(),
                "ignoring a non-HTTP proxyUrl"
            );
            return None;
        }
        Err(err) => {
            tracing::warn!(
                proxy,
                err = js_value_to_string(err),
                "ignoring an invalid proxyUrl"
            );
            return None;
        }
    };
    let mut base = url.href();
    if !base.ends_with('/') {
        base.push('/');
    }
    Some(base)
}

/// The WebSocket URL through the proxy at `base` that tunnels to the TCP server at
/// `host:port`.
///
/// # Errors
///
/// Returns the browser's exception when it cannot build the URL.
pub(super) fn tcp_url(base: &str, host: &str, port: u16) -> Result<String, JsValue> {
    let url = web_sys::Url::new_with_base("tcp", base)?;
    let protocol = if url.protocol() == "https:" {
        "wss:"
    } else {
        "ws:"
    };
    url.set_protocol(protocol);
    let query = url.search_params();
    query.set("host", host);
    query.set("port", &port.to_string());
    Ok(url.href())
}
