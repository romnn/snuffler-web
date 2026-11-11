//! Thin wrappers over the DOM calls the adapter makes.
//!
//! A failed attribute or style write is logged rather than returned: one bad write must not
//! abort a whole projection pass.

use wasm_bindgen::JsCast as _;
use web_sys::{Document, HtmlElement};

use crate::Error;

pub(super) fn create_element(document: &Document, tag: &str) -> Result<HtmlElement, Error> {
    document
        .create_element(tag)
        .map_err(|err| Error::Dom(format!("{err:?}")))?
        .dyn_into::<HtmlElement>()
        .map_err(|element| Error::Dom(format!("not an HTML element: {element:?}")))
}

pub(super) fn set_style(element: &HtmlElement, properties: &[(&str, &str)]) {
    let style = element.style();
    for (name, value) in properties {
        if let Err(err) = style.set_property(name, value) {
            log::warn!("accesskit-dom: could not set style {name}: {err:?}");
        }
    }
}

pub(super) fn set_attribute(element: &HtmlElement, name: &str, value: &str) {
    if let Err(err) = element.set_attribute(name, value) {
        log::warn!("accesskit-dom: could not set attribute {name}: {err:?}");
    }
}

pub(super) fn remove_attribute(element: &HtmlElement, name: &str) {
    if let Err(err) = element.remove_attribute(name) {
        log::warn!("accesskit-dom: could not remove attribute {name}: {err:?}");
    }
}
