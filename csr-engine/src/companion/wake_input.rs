//! The scroll that wakes a resting pane, carried over the exec into the ordinary pane.
//!
//! A resting pane (`dormant.rs`) reads the bytes that woke it and whatever follows them, and
//! hands them to the `--watch` image it execs in [`VARIABLE`], hex-encoded. The new image takes
//! them out of its environment once, at start, before any thread runs ([`take_from_env`]), so a
//! later exec of its own (a rest, a new engine, a new session) never carries them again: each
//! is delivered exactly once. The watch loop replays them after its first frame is on screen,
//! as if they had been scrolled then.
//!
//! What is replayed: scrolls, as scrolled. Everything else only wakes. A key is dropped: the
//! person pressed it on the resting card, and on the new frame a key can open an item, answer a
//! dream, dismiss an item or send text into the Claude pane, none of which they could see yet.
//! A click is dropped (it was on the card, not on what the first frame draws at that spot), and
//! so is a focus report. [`parse`] still reads keys, arrows, the terminal's own string replies and
//! old-style mouse reports whole, so a scroll after them is found and none of their bytes turns
//! into a scroll.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use std::sync::Mutex;

/// Where the resting pane puts the bytes for the exec back.
pub const VARIABLE: &str = "CSR_PANE_WAKE_INPUT";
/// The most bytes carried: a few keystrokes' worth.
pub const MOST: usize = 256;

static TAKEN: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// The value for [`VARIABLE`]: the bytes, hex-encoded, at most [`MOST`] of them.
pub fn encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(MOST)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The bytes [`encode`] wrote; None for text that is not that.
pub fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || text.len() > 2 * MOST {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Read [`VARIABLE`] and take it out of the environment. Call once, before any thread starts.
pub fn take_from_env() {
    let Some(text) = std::env::var_os(VARIABLE) else {
        return;
    };
    std::env::remove_var(VARIABLE);
    let bytes = text.to_str().and_then(decode_hex).unwrap_or_default();
    *TAKEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(bytes);
}

/// The events to replay, once: empty on every later call.
pub fn take_events() -> Vec<Event> {
    let bytes = TAKEN.lock().unwrap_or_else(|e| e.into_inner()).take();
    bytes.map(|b| replayed(&b)).unwrap_or_default()
}

/// The events in `bytes` that are replayed: the scrolls. A key, a click or a focus report only
/// wakes (see the module notes).
pub fn replayed(bytes: &[u8]) -> Vec<Event> {
    parse(bytes)
        .into_iter()
        .filter(|event| {
            matches!(event, Event::Mouse(m) if matches!(
                m.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ))
        })
        .collect()
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

/// The terminal input a pane in raw mode with mouse and focus reports on can get: keys, arrows,
/// SGR mouse reports and focus reports. Anything else is skipped.
pub fn parse(bytes: &[u8]) -> Vec<Event> {
    let mut out = vec![];
    let mut rest = bytes;
    while let Some(&first) = rest.first() {
        if first == 0x1b {
            if let Some(after) = rest.strip_prefix(b"\x1b[<") {
                let end = after.iter().position(|b| matches!(b, b'M' | b'm'));
                let Some(end) = end else { break };
                out.extend(sgr_mouse(&after[..end], after[end] == b'M'));
                rest = &after[end + 1..];
                continue;
            }
            if let Some(after) = rest.strip_prefix(b"\x1b[M") {
                // The old X10 mouse report: three raw bytes, which are not keys.
                let Some(after) = after.get(3..) else { break };
                rest = after;
                continue;
            }
            if let Some(after) = string_report(rest) {
                rest = after;
                continue;
            }
            if let Some(after) = rest.strip_prefix(b"\x1b[") {
                let end = after.iter().position(|b| (0x40..=0x7e).contains(b));
                let Some(end) = end else { break };
                let code = match (&after[..end], after[end]) {
                    (b"", b'A') => Some(key(KeyCode::Up, KeyModifiers::NONE)),
                    (b"", b'B') => Some(key(KeyCode::Down, KeyModifiers::NONE)),
                    (b"", b'C') => Some(key(KeyCode::Right, KeyModifiers::NONE)),
                    (b"", b'D') => Some(key(KeyCode::Left, KeyModifiers::NONE)),
                    (b"", b'Z') => Some(key(KeyCode::BackTab, KeyModifiers::SHIFT)),
                    (b"", b'I') => Some(Event::FocusGained),
                    (b"", b'O') => Some(Event::FocusLost),
                    _ => None,
                };
                out.extend(code);
                rest = &after[end + 1..];
                continue;
            }
            if let Some(after) = rest.strip_prefix(b"\x1bO") {
                // SS3: the arrows in application-cursor mode; anything else is skipped.
                let Some(&last) = after.first() else { break };
                let code = match last {
                    b'A' => Some(KeyCode::Up),
                    b'B' => Some(KeyCode::Down),
                    b'C' => Some(KeyCode::Right),
                    b'D' => Some(KeyCode::Left),
                    _ => None,
                };
                out.extend(code.map(|code| key(code, KeyModifiers::NONE)));
                rest = &after[1..];
                continue;
            }
            match rest.get(1) {
                // Alt and a key: one event, never an Esc and then the key.
                Some(&next) if next != 0x1b => {
                    let len = utf8_len(next).min(rest.len() - 1);
                    let c = std::str::from_utf8(&rest[1..1 + len])
                        .ok()
                        .and_then(|s| s.chars().next());
                    out.extend(c.map(|c| key(KeyCode::Char(c), KeyModifiers::ALT)));
                    rest = &rest[1 + len.max(1)..];
                }
                _ => {
                    out.push(key(KeyCode::Esc, KeyModifiers::NONE));
                    rest = &rest[1..];
                }
            }
            continue;
        }
        let (event, used) = match first {
            b'\t' => (Some(key(KeyCode::Tab, KeyModifiers::NONE)), 1),
            b'\r' | b'\n' => (Some(key(KeyCode::Enter, KeyModifiers::NONE)), 1),
            0x01..=0x1a => {
                let c = char::from(b'a' + first - 1);
                (Some(key(KeyCode::Char(c), KeyModifiers::CONTROL)), 1)
            }
            _ => {
                let len = utf8_len(first).min(rest.len());
                let c = std::str::from_utf8(&rest[..len])
                    .ok()
                    .and_then(|s| s.chars().next());
                (
                    c.map(|c| key(KeyCode::Char(c), KeyModifiers::NONE)),
                    len.max(1),
                )
            }
        };
        out.extend(event);
        rest = &rest[used..];
    }
    out
}

/// What follows a terminal's own string reply (OSC, DCS, APC, PM, SOS: `ESC` and one of `] P _ ^ X`,
/// up to `ESC \` or BEL), none of it keys. None when `bytes` does not start with one. One that
/// never ends (cut off, or Alt and a key) takes the rest with it: nothing after it is replayed.
fn string_report(bytes: &[u8]) -> Option<&[u8]> {
    let body = match bytes {
        [0x1b, b']' | b'P' | b'_' | b'^' | b'X', body @ ..] => body,
        _ => return None,
    };
    let end = body.iter().enumerate().find_map(|(i, &b)| match b {
        0x07 => Some(i + 1),
        0x1b if body.get(i + 1) == Some(&b'\\') => Some(i + 2),
        _ => None,
    });
    Some(&body[end.unwrap_or(body.len())..])
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xf0..=0xf7 => 4,
        0xe0..=0xef => 3,
        0xc0..=0xdf => 2,
        _ => 1,
    }
}

/// One SGR mouse report: `button;column;row`, 1-based, pressed (`M`) or released (`m`).
fn sgr_mouse(body: &[u8], pressed: bool) -> Option<Event> {
    let text = std::str::from_utf8(body).ok()?;
    let mut fields = text.split(';').map(str::parse::<u16>);
    let (button, column, row) = (
        fields.next()?.ok()?,
        fields.next()?.ok()?,
        fields.next()?.ok()?,
    );
    // The modifier bits come off before the button is read: 69 is Shift and wheel down, 80 is
    // Ctrl and wheel up. Motion (32) stays in, so a drag is still no button.
    let mut modifiers = KeyModifiers::NONE;
    for (bit, modifier) in [
        (SGR_SHIFT, KeyModifiers::SHIFT),
        (SGR_META, KeyModifiers::ALT),
        (SGR_CTRL, KeyModifiers::CONTROL),
    ] {
        if button & bit != 0 {
            modifiers |= modifier;
        }
    }
    let button = button & !(SGR_SHIFT | SGR_META | SGR_CTRL);
    let kind = match (button, pressed) {
        (64, _) => MouseEventKind::ScrollUp,
        (65, _) => MouseEventKind::ScrollDown,
        (0, true) => MouseEventKind::Down(crossterm::event::MouseButton::Left),
        (0, false) => MouseEventKind::Up(crossterm::event::MouseButton::Left),
        _ => return None,
    };
    Some(Event::Mouse(MouseEvent {
        kind,
        column: column.saturating_sub(1),
        row: row.saturating_sub(1),
        modifiers,
    }))
}

/// The modifier bits of an SGR mouse button code.
const SGR_SHIFT: u16 = 4;
const SGR_META: u16 = 8;
const SGR_CTRL: u16 = 16;

/// How many lines one wheel notch moves the page: up is negative. 0 for anything that is not a
/// wheel. The watch loop and the replay of a scroll that woke the pane both move the page by this.
pub fn scroll_step(kind: MouseEventKind) -> isize {
    match kind {
        MouseEventKind::ScrollUp => -3,
        MouseEventKind::ScrollDown => 3,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(events: &[Event]) -> Vec<KeyCode> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Key(k) => Some(k.code),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_bytes_survive_the_trip_through_the_environment_value() {
        let bytes = b"t\x1b[<64;3;4M\xc3\xa9";
        assert_eq!(decode_hex(&encode(bytes)).unwrap(), bytes);
        assert_eq!(encode(&[7u8; 1000]).len(), 2 * MOST);
        assert!(decode_hex("abc").is_none());
        assert!(decode_hex("zz").is_none());
    }

    fn scrolls(events: &[Event]) -> Vec<(MouseEventKind, u16, u16)> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Mouse(m) => Some((m.kind, m.column, m.row)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn only_scrolls_are_replayed_and_keys_clicks_and_focus_only_wake() {
        let events = replayed(b"\x1b[It\x1b[B\x1b[<65;10;5M\x1b[<0;3;4M\x1b[<0;3;4mq\x03\xc3\xa9");
        assert_eq!(scrolls(&events), [(MouseEventKind::ScrollDown, 9, 4)]);
        assert_eq!(events.len(), 1, "{events:?}");
        // A key on the new frame can open an item (`o`), then dismiss it (`x`) or send text
        // into the Claude pane (`s`); a digit answers a dream. None of them is acted on.
        for typed in [
            &b"o"[..],
            b"ox",
            b"os",
            b"1",
            b"b",
            b"q",
            b"Q",
            b"\x03",
            b"\x1b",
            b"\r ",
            b"\x1bq",
            b"\x1b[B",
            b"\x1bOB",
            b"\x1b[<0;3;4M",
            b"\x1b[I",
            b"\x1b[O",
        ] {
            assert!(replayed(typed).is_empty(), "{typed:?}");
        }
    }

    #[test]
    fn the_parser_reads_keys_and_sequences_whole_so_no_byte_becomes_a_scroll() {
        let all = parse(b"\x1b[It\x1b[B\x1b[<65;10;5M\xc3\xa9");
        assert_eq!(
            keys(&all),
            [KeyCode::Char('t'), KeyCode::Down, KeyCode::Char('é')]
        );
        assert_eq!(keys(&parse(b"\x1bOB")), [KeyCode::Down]);
        assert!(keys(&parse(b"\x1bOP")).is_empty(), "SS3 F1 is skipped");
        assert_eq!(keys(&parse(b"\x1b\x1b")), [KeyCode::Esc, KeyCode::Esc]);
        assert_eq!(keys(&parse(b"\r ")), [KeyCode::Enter, KeyCode::Char(' ')]);
        // Alt-q is one key, never an Esc and then a q.
        let alt = parse(b"\x1bq");
        assert!(matches!(&alt[..], [Event::Key(k)]
            if k.code == KeyCode::Char('q') && k.modifiers.contains(KeyModifiers::ALT)));
        // A scroll after keys is still found.
        assert_eq!(
            scrolls(&replayed(b"ox\x1bq\x1bOB\x1b[<64;3;4M")),
            [(MouseEventKind::ScrollUp, 2, 3)]
        );
    }

    #[test]
    fn the_terminal_s_own_replies_are_never_keys_or_scrolls() {
        // A kitty graphics reply, an OSC reply and an X10 mouse report carry letters and
        // spaces that must not turn into keys.
        for report in [
            &b"\x1b_Gi=1;OK\x1b\\"[..],
            b"\x1b]11;rgb:0/0/0\x07",
            b"\x1b[M #!",
        ] {
            assert!(parse(report).is_empty(), "{report:?}");
            let mut typed = report.to_vec();
            typed.push(b't');
            assert_eq!(keys(&parse(&typed)), [KeyCode::Char('t')], "{report:?}");
            let mut scrolled = report.to_vec();
            scrolled.extend_from_slice(b"\x1b[<65;1;1M");
            assert_eq!(
                scrolls(&replayed(&scrolled)),
                [(MouseEventKind::ScrollDown, 0, 0)],
                "{report:?}"
            );
        }
        // A string that never ends takes what follows it, and replays none of it.
        assert!(parse(b"\x1b_abc\x1b[<65;1;1M").is_empty());
    }

    #[test]
    fn the_wheel_reads_through_the_modifier_bits() {
        use MouseEventKind::{ScrollDown, ScrollUp};
        // 64 and 65 plain; 68 and 69 with Shift; 80 and 81 with Ctrl.
        for (button, kind, modifiers) in [
            (64, ScrollUp, KeyModifiers::NONE),
            (65, ScrollDown, KeyModifiers::NONE),
            (68, ScrollUp, KeyModifiers::SHIFT),
            (69, ScrollDown, KeyModifiers::SHIFT),
            (72, ScrollUp, KeyModifiers::ALT),
            (73, ScrollDown, KeyModifiers::ALT),
            (80, ScrollUp, KeyModifiers::CONTROL),
            (81, ScrollDown, KeyModifiers::CONTROL),
            (
                93,
                ScrollDown,
                KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL,
            ),
        ] {
            let report = format!("\x1b[<{button};10;5M");
            let events = replayed(report.as_bytes());
            let [Event::Mouse(m)] = &events[..] else {
                panic!("button {button} is a scroll: {events:?}");
            };
            assert_eq!(
                (m.kind, m.column, m.row, m.modifiers),
                (kind, 9, 4, modifiers),
                "button {button}"
            );
        }
        // A click with Shift is still a click (dropped on replay), and a drag or the sideways
        // wheel is still no scroll.
        for other in [4, 32, 36, 66, 67, 70, 71, 96] {
            let report = format!("\x1b[<{other};10;5M");
            assert!(replayed(report.as_bytes()).is_empty(), "button {other}");
        }
        assert_eq!(scroll_step(MouseEventKind::ScrollUp), -3);
        assert_eq!(scroll_step(MouseEventKind::ScrollDown), 3);
        assert_eq!(scroll_step(MouseEventKind::Moved), 0);
    }

    #[test]
    fn the_environment_value_is_taken_once_and_hands_out_its_scroll_once_and_a_key_never() {
        // This checks the hand-out only. That the page moves once is `pane_tests`'s
        // `a_scroll_that_woke_a_resting_pane_moves_the_page_once`.
        // The key `b` (the theme toggle) woke the pane: nothing of it is replayed.
        std::env::set_var(VARIABLE, encode(b"b"));
        take_from_env();
        let key_gone = std::env::var_os(VARIABLE).is_none();
        let after_key = take_events();
        // A scroll woke the pane, with a key typed after it: the scroll once, the key never.
        std::env::set_var(VARIABLE, encode(b"\x1b[<64;3;4Mb"));
        take_from_env();
        // Read everything first, so a failure names every part that broke.
        let gone = std::env::var_os(VARIABLE).is_none();
        let first = scrolls(&take_events());
        let second = take_events();
        take_from_env();
        let third = take_events();
        assert!(
            key_gone
                && after_key.is_empty()
                && gone
                && first == [(MouseEventKind::ScrollUp, 2, 3)]
                && second.is_empty()
                && third.is_empty(),
            "key_left_the_environment={key_gone} after_key={after_key:?} (a key is never \
             acted on) left_the_environment={gone} (an exec after this carries none) \
             first={first:?} second={second:?} (never twice) third={third:?}"
        );
    }
}
