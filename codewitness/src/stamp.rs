use std::fmt;

const RAW_PREFIX: &str = "b3";
const NORMALIZED_PREFIX: &str = "b3n";

/// Which hashing discipline produced a [`Stamp`]. Two stamps of different
/// kinds are never meaningfully comparable for equality even if the
/// underlying content happens to collide — [`Stamp`]'s `PartialEq` already
/// encodes this because the kind is embedded in the formatted string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampKind {
    /// [`Stamp::from_bytes`]: raw bytes, no normalization whatsoever.
    Raw,
    /// [`stamp_normalized`]: whitespace-insensitive.
    Normalized,
}

impl StampKind {
    /// Recompute a stamp of this kind over `bytes`.
    pub fn compute(self, bytes: &[u8]) -> Stamp {
        match self {
            StampKind::Raw => Stamp::from_bytes(bytes),
            StampKind::Normalized => stamp_normalized(bytes),
        }
    }
}

/// A versioned content hash: `"b3:<64 hex chars>"` for raw content,
/// `"b3n:<64 hex chars>"` for whitespace-normalized content. The prefix is
/// load-bearing — it is how [`StampKind::compute`] knows which discipline to
/// replay when re-stamping content for comparison, and it prevents a raw
/// stamp and a normalized stamp from ever comparing equal by accident.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Stamp(String);

impl Stamp {
    /// Hash `bytes` verbatim. This constructor normalizes **nothing**: two
    /// byte-identical-except-for-a-trailing-space inputs produce different
    /// stamps. Use [`stamp_normalized`] when that sensitivity is unwanted.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let hash = blake3::hash(bytes);
        Self(format!("{RAW_PREFIX}:{}", hash.to_hex()))
    }

    /// Which construction path produced this stamp.
    pub fn kind(&self) -> StampKind {
        if self.0.starts_with(NORMALIZED_PREFIX)
            && self.0.as_bytes().get(NORMALIZED_PREFIX.len()) == Some(&b':')
        {
            StampKind::Normalized
        } else {
            StampKind::Raw
        }
    }

    /// The canonical `"b3:<hex>"` / `"b3n:<hex>"` string form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Stamp {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Hash `bytes` after whitespace-insensitive normalization, for claims that
/// should survive pure reformatting (tabs-to-spaces, trailing whitespace,
/// re-indentation). Normalization rules, applied line by line:
///
/// 1. Runs of interior whitespace collapse to a single ASCII space.
/// 2. Leading/trailing whitespace on each line is trimmed.
/// 3. Trailing blank lines are dropped.
///
/// This is deliberately conservative: it does not reflow code, reorder
/// statements, or understand any language grammar. It is whitespace
/// insensitivity, not semantic equivalence.
///
/// Normalization is **byte-level**, not text-level: it never decodes
/// `bytes` as UTF-8 (no `String::from_utf8_lossy`). Lossy UTF-8 decoding
/// maps every invalid byte sequence to the same replacement character
/// (U+FFFD), which would make genuinely different, non-UTF-8 content
/// (binary data, or text in another encoding) collapse to identical
/// stamps — silently breaking the "stamp identifies content" contract for
/// exactly the inputs where a content hash matters most. "Whitespace"
/// here means exactly four ASCII bytes: space (`b' '`), tab (`b'\t'`),
/// carriage return (`b'\r'`), and line feed (`b'\n'`) — nothing else, and
/// nothing Unicode-aware.
pub fn stamp_normalized(bytes: &[u8]) -> Stamp {
    let normalized = normalize_whitespace_bytes(bytes);
    let hash = blake3::hash(&normalized);
    Stamp(format!("{NORMALIZED_PREFIX}:{}", hash.to_hex()))
}

fn is_normalization_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

fn normalize_whitespace_bytes(bytes: &[u8]) -> Vec<u8> {
    // Split on `\n` only, mirroring the line model everywhere else in this
    // crate; a lone `\r` is treated as interior whitespace on the line it
    // appears in (see `diff_id.rs` for the separate CRLF-aware splitter
    // used by diffing, which treats `\r\n` as a single line terminator).
    let mut lines: Vec<Vec<u8>> = bytes
        .split(|&b| b == b'\n')
        .map(collapse_line_whitespace)
        .collect();

    while matches!(lines.last(), Some(l) if l.is_empty()) {
        lines.pop();
    }

    let mut out = Vec::with_capacity(bytes.len());
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        out.extend_from_slice(line);
    }
    out
}

/// Collapse runs of [`is_normalization_whitespace`] bytes within a single
/// line to a single ASCII space, and drop leading/trailing runs entirely.
fn collapse_line_whitespace(line: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(line.len());
    let mut in_run = false;
    for &b in line {
        if is_normalization_whitespace(b) {
            in_run = true;
        } else {
            if in_run && !out.is_empty() {
                out.push(b' ');
            }
            in_run = false;
            out.push(b);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_stamp_is_sensitive_to_whitespace() {
        let a = Stamp::from_bytes(b"fn main() {}\n");
        let b = Stamp::from_bytes(b"fn main() {}  \n");
        assert_ne!(a, b);
        assert_eq!(a.kind(), StampKind::Raw);
    }

    #[test]
    fn normalized_stamp_never_lossily_collapses_invalid_utf8() {
        // `String::from_utf8_lossy` maps every invalid byte sequence to the
        // same U+FFFD replacement character — these two distinct
        // (deliberately invalid-UTF-8) byte strings would previously have
        // decoded to the same lossy text (replacement-char + "a") and so
        // hashed identically. Byte-level normalization must keep them
        // distinct.
        let a = stamp_normalized(&[0xFF, b'a']);
        let b = stamp_normalized(&[0xFE, b'a']);
        assert_ne!(a, b);
    }

    #[test]
    fn normalized_stamp_ignores_whitespace_reflow() {
        let a = stamp_normalized(b"fn  main()  {\n    1\n}\n");
        let b = stamp_normalized(b"fn main() {\n1\n}");
        assert_eq!(a, b);
        assert_eq!(a.kind(), StampKind::Normalized);
    }

    #[test]
    fn raw_and_normalized_never_collide() {
        let raw = Stamp::from_bytes(b"same");
        let norm = stamp_normalized(b"same");
        assert_ne!(raw, norm);
    }

    #[test]
    fn format_prefixes_are_stable() {
        let raw = Stamp::from_bytes(b"x");
        assert!(raw.as_str().starts_with("b3:"));
        let norm = stamp_normalized(b"x");
        assert!(norm.as_str().starts_with("b3n:"));
    }

    #[test]
    fn stamp_kind_requires_the_complete_normalized_prefix() {
        let incomplete_prefix = Stamp("b3n-not-a-normalized-stamp".to_owned());

        assert_eq!(incomplete_prefix.kind(), StampKind::Raw);
    }

    #[test]
    fn display_and_as_ref_expose_the_canonical_stamp() {
        let stamp = Stamp::from_bytes(b"display contract");

        assert_eq!(stamp.to_string(), stamp.as_str());
        assert_eq!(AsRef::<str>::as_ref(&stamp), stamp.as_str());
    }

    #[test]
    fn normalized_stamp_preserves_every_noninitial_line_boundary() {
        assert_eq!(
            normalize_whitespace_bytes(b"alpha\nbeta\ngamma"),
            b"alpha\nbeta\ngamma"
        );
    }
}
