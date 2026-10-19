//! The text agents.ts keeps: cut by code points, cleaned of terminal control, and redacted.
//!
//! Every character class here is JavaScript's, not Rust's: `\s` and `trim` have a different set of
//! white space, `\b` and `i` are ASCII in a pattern with no `u` flag, and `.` stops at four line
//! terminators. `redact_private` (the private writer's redaction) is a close port with the Rust
//! defaults, so it is not used for what the agents file keeps.

use regex::{Captures, Regex};
use serde_json::Value;
use std::sync::LazyLock;

/// What JavaScript's `\s` (and `String.prototype.trim`) holds, as the inside of a character class.
pub const WS: &str = concat!(
    r"\t\n\x0b\x0c\r \x{a0}\x{1680}\x{2000}-\x{200a}",
    r"\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}"
);
const REDACTED: &str = "[redacted]";

/// Whether JavaScript counts `c` as white space.
pub fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\x0b' | '\x0c' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `String.prototype.trim`.
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

/// `text.replace(/\s+/g, ' ')`.
pub fn squash_space(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if is_js_space(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// At most `most` code points; a longer text is cut to `most - 1` and ends in an ellipsis.
pub fn cap(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    let mut out: String = text.chars().take(most.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The bytes the text takes in UTF-8.
pub fn utf8_length(text: &str) -> usize {
    text.len()
}

fn build(pattern: &str) -> Regex {
    let pattern = pattern.replace("@WS@", WS);
    Regex::new(&pattern).unwrap_or_else(|e| panic!("a fixed pattern: {pattern}: {e}"))
}

/// A word with each ASCII letter as a two-letter class: a case-blind match that stays ASCII.
fn ci(word: &str) -> String {
    word.chars()
        .map(|c| match c.is_ascii_alphabetic() {
            true => format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase()),
            false => c.to_string(),
        })
        .collect()
}

static TERMINAL: LazyLock<Regex> = LazyLock::new(|| {
    build(concat!(
        r"\x1b\[[0-?]*[ -/]*[@-~]",
        r"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)?",
        r"|\x1b[@-Z\\-_]",
        r"|\x1b[^\n\r\x{2028}\x{2029}]",
        r"|[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]",
    ))
});

/// Terminal escape sequences and control characters out of a text: CSI, OSC (BEL or ST ended),
/// other two-character escapes, the C1 range, and the C0 controls but newline and tab.
pub fn strip_terminal(text: &str) -> String {
    TERMINAL.replace_all(text, "").into_owned()
}

static SECRETS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?s)-----BEGIN [A-Z0-9 ]+-----.*?(?:-----END [A-Z0-9 ]+-----|\z)",
        r"(?-u:\b)sk-[A-Za-z0-9_\-]{16,}",
        r"(?-u:\b)xai-[A-Za-z0-9]{16,}",
        r"(?-u:\b)gh[pousr]_[A-Za-z0-9]{20,}",
        r"(?-u:\b)github_pat_[A-Za-z0-9_]{20,}",
        r"(?-u:\b)(?:AKIA|ASIA)[A-Z0-9]{16}(?-u:\b)",
        r"(?-u:\b)xox[abposr]-[A-Za-z0-9\-]{10,}",
        r"(?-u:\b)AIza[0-9A-Za-z_\-]{35}",
        r"(?-u:\b)(?:hf|npm)_[A-Za-z0-9]{30,}",
        r"(?-u:\b)eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
    ]
    .iter()
    .map(|pattern| build(pattern))
    .collect()
});

const CRED: &str = r"[A-Za-z0-9._~+/=\-]";

static AUTHORIZATION: LazyLock<Regex> = LazyLock::new(|| {
    build(&format!(
        r#"(?-u:\b)({}["']?[@WS@]*[:=][@WS@]*["']?(?:{}|{}|{})[@WS@]+){CRED}{{8,}}"#,
        ci("authorization"),
        ci("bearer"),
        ci("basic"),
        ci("token"),
    ))
});

static BEARER: LazyLock<Regex> =
    LazyLock::new(|| build(&format!(r"(?-u:\b)({}[@WS@]+)({CRED}{{8,}})", ci("bearer"))));

static KEYED: LazyLock<Regex> = LazyLock::new(|| {
    let name = |a: &str, b: &str| format!("{}[_-]?{}", ci(a), ci(b));
    let names = [
        ci("password"),
        ci("passwd"),
        ci("secret"),
        ci("token"),
        name("api", "key"),
        name("access", "key"),
        name("private", "key"),
        name("client", "secret"),
    ]
    .join("|");
    build(&format!(
        concat!(
            r#"(?-u:\b)([A-Za-z0-9_.\-]*?(?:{names})["']?[@WS@]*[=:][@WS@]*)"#,
            r#"("[^"\n]*"|'[^'\n]*'|[^@WS@,;&"'`)\]}}]+)"#
        ),
        names = names
    ))
});

static ENV_LINE: LazyLock<Regex> = LazyLock::new(|| {
    build(concat!(
        r"^([ \t]*(?:export[ \t]+)?[A-Z][A-Z0-9_]*(?:KEY|TOKEN|SECRET)[A-Z0-9_]*[ \t]*[=:][ \t]*)",
        r"([^@WS@].*)$",
    ))
});

static HEX_RUN: LazyLock<Regex> = LazyLock::new(|| build(r"(?-u:\b)[0-9a-fA-F]{32,}(?-u:\b)"));

/// A value of six characters (UTF-16 units) or more with a digit or one of `@#$%^*!~+=`.
fn looks_secret(value: &str) -> bool {
    value.encode_utf16().count() >= 6
        && value
            .chars()
            .any(|c| c.is_ascii_digit() || "@#$%^*!~+=".contains(c))
}

/// One line (no terminator in it) of an `export NAME_KEY=value` assignment, value redacted.
fn env_line(line: &str) -> String {
    let Some(found) = ENV_LINE.captures(line) else {
        return line.to_string();
    };
    let value = &found[2];
    let marked = value
        .strip_prefix(['\'', '"'])
        .unwrap_or(value)
        .starts_with(REDACTED);
    if marked {
        return line.to_string();
    }
    format!("{}{REDACTED}", &found[1])
}

/// `^...$` with the `m` flag, as JavaScript reads it: a line ends at \n, \r, U+2028 or U+2029.
fn env_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    for (at, c) in text.char_indices() {
        if matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            out.push_str(&env_line(&text[from..at]));
            out.push(c);
            from = at + c.len_utf8();
        }
    }
    out.push_str(&env_line(&text[from..]));
    out
}

/// Credentials out of a text. Applying it twice changes nothing more.
pub fn redact_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = text.to_string();
    for pattern in SECRETS.iter() {
        out = pattern.replace_all(&out, REDACTED).into_owned();
    }
    out = AUTHORIZATION
        .replace_all(&out, format!("${{1}}{REDACTED}").as_str())
        .into_owned();
    out = BEARER
        .replace_all(&out, |c: &Captures<'_>| {
            let run = &c[2];
            match run
                .chars()
                .any(|c| c.is_ascii_digit() || "_~+/=".contains(c))
            {
                true => format!("{}{REDACTED}", &c[1]),
                false => c[0].to_string(),
            }
        })
        .into_owned();
    let seen = out.clone();
    out = KEYED
        .replace_all(&seen, |c: &Captures<'_>| {
            let value = c.get(2).expect("the value group");
            let head = &c[1];
            if let Some(quote) = value.as_str().strip_prefix(['"', '\'']).map(|_| &c[2][..1]) {
                return format!("{head}{quote}{REDACTED}{quote}");
            }
            // `(?!\[redacted\])`: a value already redacted is not a match at all.
            if seen[value.start()..].starts_with(REDACTED) {
                return c[0].to_string();
            }
            match looks_secret(value.as_str()) {
                true => format!("{head}{REDACTED}"),
                false => c[0].to_string(),
            }
        })
        .into_owned();
    out = env_lines(&out);
    HEX_RUN.replace_all(&out, REDACTED).into_owned()
}

/// Every string inside JSON-shaped data redacted and cleaned of terminal control; keys and
/// numbers stay.
pub fn redact_agent_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_text(&strip_terminal(text))),
        Value::Array(items) => Value::Array(items.iter().map(redact_agent_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, inner)| (key.clone(), redact_agent_value(inner)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A string through `redact_agent_value`.
pub fn redact_str(text: &str) -> String {
    redact_text(&strip_terminal(text))
}

/// One line of the agent's own words for a row: redacted first, then cut (a secret cut in half is
/// no longer matched).
pub fn one_line(text: &str, most: usize) -> String {
    cap(js_trim(&squash_space(&redact_str(text))), most)
}

/// A short fingerprint of a path (cyrb53, base 36): membership of a file in an agent's writes
/// without keeping the file. The arithmetic is JavaScript's `Math.imul` over UTF-16 code units.
pub fn fingerprint(path: &str) -> String {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    for unit in path.encode_utf16() {
        let code = u32::from(unit);
        h1 = (h1 ^ code).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ code).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    let mut n = 4_294_967_296 * u64::from(2_097_151 & h2) + u64::from(h1);
    if n == 0 {
        return "0".into();
    }
    let mut digits = Vec::new();
    while n > 0 {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(n % 36) as usize]);
        n /= 36;
    }
    digits.reverse();
    String::from_utf8(digits).expect("ASCII digits")
}

/// An absolute path with `.` and `..` resolved; a relative one is taken from `cwd`.
pub fn normalize_path(path: &str, cwd: &str) -> String {
    let joined = match path.starts_with('/') {
        true => path.to_string(),
        false => format!("{}/{path}", cwd.trim_end_matches('/')),
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            ".." => {
                parts.pop();
            }
            "" | "." => (),
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

/// `^[A-Za-z0-9_.:@-]{1,128}$`.
pub fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:@-".contains(&b))
}

/// `^[A-Za-z0-9_-]{1,128}$`: what may name a session's file.
pub fn session_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
