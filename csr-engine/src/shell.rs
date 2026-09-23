//! POSIX shell quoting for paths we hand to a shell.
//!
//! Two places need it and must agree: hook commands in `~/.claude/settings.json`
//! (Claude Code runs each entry through a shell) and the recovery commands setup
//! prints for a human to paste. An install under `/Users/me/CSR Tools/bin` would
//! otherwise produce a hook that tries to run `/Users/me/CSR`, and setup would
//! report activation while every hook exited 127.

/// A path made only of these needs no quoting, which keeps the common case
/// readable in settings.json and in printed advice.
fn is_shell_safe(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))
}

/// Single-quote `value` for POSIX `sh` unless it is already safe bare.
///
/// Inside single quotes every character is literal, so only `'` itself needs
/// handling: close the quote, emit an escaped quote, reopen — the usual
/// `'\''`. That covers spaces, `$`, backticks, `;`, backslashes and newlines.
pub fn shell_quote(value: &str) -> String {
    if is_shell_safe(value) {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paths_stay_bare() {
        assert_eq!(
            shell_quote("/usr/local/bin/csr-engine"),
            "/usr/local/bin/csr-engine"
        );
        assert_eq!(
            shell_quote("/home/me/.local/bin/csr-engine"),
            "/home/me/.local/bin/csr-engine"
        );
    }

    #[test]
    fn awkward_paths_are_quoted() {
        assert_eq!(
            shell_quote("/Users/me/CSR Tools/bin/csr-engine"),
            "'/Users/me/CSR Tools/bin/csr-engine'"
        );
        assert_eq!(
            shell_quote("/tmp/a;rm -rf x/csr-engine"),
            "'/tmp/a;rm -rf x/csr-engine'"
        );
        assert_eq!(
            shell_quote("/tmp/$(id)/csr-engine"),
            "'/tmp/$(id)/csr-engine'"
        );
        assert_eq!(
            shell_quote("/tmp/back\\slash/csr-engine"),
            "'/tmp/back\\slash/csr-engine'"
        );
        assert_eq!(
            shell_quote("/tmp/new\nline/csr-engine"),
            "'/tmp/new\nline/csr-engine'"
        );
    }

    /// The one character that cannot be carried inside single quotes.
    #[test]
    fn apostrophes_close_and_reopen() {
        assert_eq!(
            shell_quote("/tmp/o'brien/csr-engine"),
            "'/tmp/o'\\''brien/csr-engine'"
        );
        assert_eq!(shell_quote("'"), "''\\'''");
    }

    #[test]
    fn empty_is_quoted_not_dropped() {
        assert_eq!(shell_quote(""), "''");
    }
}
