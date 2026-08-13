//! Translation of DOM keyboard input into [`egui::Event`]s.
//!
//! Keyboard input is translated here rather than left to the web runner because the
//! runner listens for keys on the canvas, and a focused proxy means the canvas does
//! not have DOM focus — its listeners never fire.
//!
//! The module is target-independent on purpose: it consumes the plain
//! [`accesskit_dom::KeyEvent`] rather than a `web-sys` handle, so the mapping is
//! unit-tested on the host.

use accesskit_dom::KeyEvent;

fn modifiers(event: &KeyEvent) -> egui::Modifiers {
    egui::Modifiers {
        alt: event.alt,
        ctrl: event.ctrl,
        shift: event.shift,
        // As in eframe's own web input: knowing whether this is a mac would let us
        // be exact, and treating meta as command is close enough in practice.
        mac_cmd: event.meta,
        command: event.ctrl || event.meta,
    }
}

/// Translate a DOM key event into egui events, reporting whether egui wants it.
///
/// Translated events are appended to `out`. A `false` return means the caller
/// should leave the browser's own handling alone — most importantly for `Tab`,
/// which has to keep moving DOM focus.
pub fn translate(event: &KeyEvent, pressed: bool, out: &mut Vec<egui::Event>) -> bool {
    let modifiers = modifiers(event);
    let mut wanted = false;

    if let Some(key) = egui::Key::from_name(&event.key) {
        out.push(egui::Event::Key {
            key,
            // eframe does not resolve physical keys on web either.
            physical_key: None,
            pressed,
            repeat: event.repeat,
            modifiers,
        });
        wanted = true;
    }

    // A printable key reports the character it produces as its whole name, so the
    // named keys ("Enter", "ArrowUp", "F3", …) are exactly those longer than one
    // scalar. Chords are shortcuts rather than text and must not be typed out.
    if pressed && !modifiers.command && !modifiers.alt {
        let mut chars = event.key.chars();
        if let (Some(ch), None) = (chars.next(), chars.next())
            && !ch.is_control()
        {
            out.push(egui::Event::Text(ch.to_string()));
            wanted = true;
        }
    }
    wanted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> KeyEvent {
        KeyEvent {
            key: name.to_owned(),
            repeat: false,
            alt: false,
            ctrl: false,
            shift: false,
            meta: false,
        }
    }

    /// A printable key both presses its egui key and types its character.
    #[test]
    fn printable_keys_produce_key_and_text() {
        let mut out = Vec::new();
        assert!(translate(&key("a"), true, &mut out));
        assert!(matches!(
            out.first(),
            Some(egui::Event::Key {
                key: egui::Key::A,
                pressed: true,
                ..
            })
        ));
        assert!(out.iter().any(|event| matches!(
            event,
            egui::Event::Text(text) if text == "a"
        )));
    }

    /// A named key ("Enter") presses its egui key but must not type its name out.
    #[test]
    fn named_keys_produce_no_text() {
        let mut out = Vec::new();
        assert!(translate(&key("Enter"), true, &mut out));
        assert!(
            out.iter()
                .all(|event| !matches!(event, egui::Event::Text(_)))
        );
    }

    /// A chord is a shortcut, not typing: Ctrl+C must not insert a "c".
    #[test]
    fn chords_produce_no_text() {
        let mut out = Vec::new();
        let mut chord = key("c");
        chord.ctrl = true;
        assert!(translate(&chord, true, &mut out));
        assert!(
            out.iter()
                .all(|event| !matches!(event, egui::Event::Text(_)))
        );
    }

    /// Releases repeat the key event only; text is typed on press alone.
    #[test]
    fn releases_produce_no_text() {
        let mut out = Vec::new();
        assert!(translate(&key("a"), false, &mut out));
        assert!(matches!(
            out.first(),
            Some(egui::Event::Key { pressed: false, .. })
        ));
        assert_eq!(out.len(), 1);
    }

    /// A key egui has no name for (a printable non-ASCII character) still types.
    #[test]
    fn unnamed_printable_keys_still_type() {
        let mut out = Vec::new();
        assert!(translate(&key("ß"), true, &mut out));
        assert!(matches!(
            out.first(),
            Some(egui::Event::Text(text)) if text == "ß"
        ));
    }
}
