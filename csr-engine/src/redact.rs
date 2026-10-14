//! Shared secret scrubber. One function decides what a credential looks like,
//! so import (chunks) and the transcript mirror redact the same bytes.
//!
//! Patterns port `.plans/release-10.1/scripts/secret_scan.py`, run on decoded
//! text instead of raw JSON bytes. A hit becomes `[secret redacted: <kind>]`;
//! the marker matches no pattern, so a second pass changes nothing. Counts
//! only, never values. `CSR_NO_SECRET_SCRUB=1` turns the scrub off for
//! false-positive debugging.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::{Captures, Regex};

/// Every kind the scrubber reports, in status order.
pub const KINDS: [&str; 12] = [
    "anthropic_key",
    "openai_key",
    "openrouter_key",
    "github_token",
    "aws_access_key",
    "slack_token",
    "google_api_key",
    "stripe_key",
    "npm_token",
    "private_key_block",
    "jwt",
    "env_secret",
];

const ANTHROPIC: usize = 0;
const OPENAI: usize = 1;
const OPENROUTER: usize = 2;
const GITHUB: usize = 3;
const AWS: usize = 4;
const SLACK: usize = 5;
const GOOGLE: usize = 6;
const STRIPE: usize = 7;
const NPM: usize = 8;
const PRIVATE_KEY: usize = 9;
const JWT: usize = 10;
const ENV_SECRET: usize = 11;

/// Per-kind hit counts, indexed like [`KINDS`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SecretCounts([usize; KINDS.len()]);

impl SecretCounts {
    pub fn total(&self) -> usize {
        self.0.iter().sum()
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|count| *count == 0)
    }

    pub fn get(&self, kind: &str) -> usize {
        KINDS
            .iter()
            .position(|name| *name == kind)
            .map_or(0, |index| self.0[index])
    }

    /// `(kind, count)` for every kind with a non-zero count.
    pub fn nonzero(&self) -> impl Iterator<Item = (&'static str, usize)> + '_ {
        KINDS
            .iter()
            .zip(self.0.iter())
            .filter(|(_, count)| **count > 0)
            .map(|(kind, count)| (*kind, *count))
    }

    pub fn add(&mut self, other: &SecretCounts) {
        for (mine, theirs) in self.0.iter_mut().zip(other.0.iter()) {
            *mine += theirs;
        }
    }

    /// Per-kind maximum: the cumulative total a file's repeated full reparses
    /// have reached.
    pub fn max_with(&self, other: &SecretCounts) -> SecretCounts {
        let mut out = *self;
        for (mine, theirs) in out.0.iter_mut().zip(other.0.iter()) {
            *mine = (*mine).max(*theirs);
        }
        out
    }

    /// `{"kind": count}` for the non-zero kinds; `None` when nothing was hit.
    pub fn to_json(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let map: serde_json::Map<String, serde_json::Value> = self
            .nonzero()
            .map(|(kind, count)| (kind.to_string(), serde_json::Value::from(count as u64)))
            .collect();
        Some(serde_json::Value::Object(map).to_string())
    }

    /// Inverse of [`to_json`](Self::to_json); unknown kinds and bad JSON read as zero.
    pub fn from_json(raw: Option<&str>) -> SecretCounts {
        let mut counts = SecretCounts::default();
        let Some(serde_json::Value::Object(map)) =
            raw.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        else {
            return counts;
        };
        for (index, kind) in KINDS.iter().enumerate() {
            counts.0[index] = map
                .get(*kind)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize;
        }
        counts
    }
}

/// Literal prefixes every pattern needs. One automaton pass over the text
/// keeps the common no-secret case cheap: import runs this over every chunk.
static PREFILTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"sk-|_live_|gh[pousr]_|github_pat_|AKIA|ASIA|xox[abprs]-|AIza|npm_|-----BEGIN|eyJ|SECRET|TOKEN|API_KEY|APIKEY|PASSWORD|PASSWD",
    )
    .expect("secret prefilter compiles")
});

/// One single-token kind. `(?m)` is unused: these never anchor on a line.
struct TokenRule {
    kind: usize,
    regex: Regex,
}

static TOKEN_RULES: LazyLock<Vec<TokenRule>> = LazyLock::new(|| {
    // Order matters: the narrow `sk-` shapes run before the generic OpenAI one,
    // which would otherwise also match them.
    let rules: [(usize, &str); 9] = [
        (ANTHROPIC, r"\bsk-ant-[A-Za-z0-9_\-]{20,}"),
        (OPENROUTER, r"\bsk-or-v1-[a-f0-9]{40,}"),
        (OPENAI, r"\bsk-(?:proj-)?[A-Za-z0-9_\-]{32,}"),
        (
            GITHUB,
            r"(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,}",
        ),
        (AWS, r"\b(?:AKIA|ASIA)[A-Z0-9]{16}"),
        (SLACK, r"xox[abprs]-[A-Za-z0-9\-]{10,}"),
        (GOOGLE, r"AIza[0-9A-Za-z_\-]{35}"),
        (STRIPE, r"(?:sk|rk)_live_[A-Za-z0-9]{20,}"),
        (NPM, r"npm_[A-Za-z0-9]{36}"),
    ];
    rules
        .into_iter()
        .map(|(kind, pattern)| TokenRule {
            kind,
            regex: Regex::new(pattern).expect("secret pattern compiles"),
        })
        .collect()
});

static JWT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"eyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}")
        .expect("jwt pattern compiles")
});

static PRIVATE_KEY_BEGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----")
        .expect("private key begin compiles")
});

static PRIVATE_KEY_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----END (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----")
        .expect("private key end compiles")
});

/// `NAME_WITH_SECRET=value` at the start of a line, optionally `export `-ed.
/// A quoted value runs to its closing quote (`PASSWORD="two words"` is one
/// value); an unquoted one to the next space. The name and `=` survive, the
/// value goes. A quote that never closes falls back to the first word.
static ENV_SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?m)^(?P<pre>[ \t]*(?:export[ \t]+)?(?:[A-Z][A-Z0-9_]*)?(?:SECRET|TOKEN|API_KEY|APIKEY|PASSWORD|PASSWD)[A-Z0-9_]*=)(?:"(?P<dq>[^"\r\n]*)"|'(?P<sq>[^'\r\n]*)'|(?P<q>["']?)(?P<val>[^\s"'\\]{8,}))"#,
    )
    .expect("env secret pattern compiles")
});

fn marker(kind: usize) -> String {
    format!("[secret redacted: {}]", KINDS[kind])
}

#[cfg(test)]
thread_local! {
    static TEST_DISABLED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Keep the unit tests off the process environment: the override is per
/// thread, so a test that flips the kill switch cannot race its neighbours.
#[cfg(test)]
pub(crate) struct DisabledForTest(Option<bool>);

#[cfg(test)]
impl DisabledForTest {
    pub(crate) fn set(disabled: bool) -> Self {
        Self(TEST_DISABLED.with(|cell| cell.replace(Some(disabled))))
    }
}

#[cfg(test)]
impl Drop for DisabledForTest {
    fn drop(&mut self) {
        TEST_DISABLED.with(|cell| cell.set(self.0));
    }
}

/// `CSR_NO_SECRET_SCRUB=1` (or `true`) switches the scrub off.
pub fn scrub_disabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = TEST_DISABLED.with(|cell| cell.get()) {
        return forced;
    }
    std::env::var("CSR_NO_SECRET_SCRUB")
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"))
        .unwrap_or(false)
}

/// Cheap test: could `text` hold a credential? False when the scrub is off.
/// A hit is not a finding, only a reason to run the full scrub: the mirror
/// uses it to leave clean transcript lines byte-identical without parsing them.
pub fn may_hold_secret(text: &str) -> bool {
    !scrub_disabled() && PREFILTER.is_match(text)
}

/// Redact secrets in `text`. Borrowed, with no allocation, when nothing matches.
pub fn redact_secrets(text: &str) -> Cow<'_, str> {
    redact_secrets_counted(text, &mut SecretCounts::default())
}

/// [`redact_secrets`], adding each hit to `counts`.
pub fn redact_secrets_counted<'a>(text: &'a str, counts: &mut SecretCounts) -> Cow<'a, str> {
    if text.len() < 8 || scrub_disabled() || !PREFILTER.is_match(text) {
        return Cow::Borrowed(text);
    }
    let mut current = Cow::Borrowed(text);
    if let Some(out) = redact_private_keys(&current, counts) {
        current = Cow::Owned(out);
    }
    for rule in TOKEN_RULES.iter() {
        if let Some(out) = redact_tokens(&current, rule, counts) {
            current = Cow::Owned(out);
        }
    }
    if let Some(out) = redact_jwt(&current, counts) {
        current = Cow::Owned(out);
    }
    if let Some(out) = redact_env_secrets(&current, counts) {
        current = Cow::Owned(out);
    }
    current
}

fn redact_tokens(text: &str, rule: &TokenRule, counts: &mut SecretCounts) -> Option<String> {
    let mut hits = 0usize;
    let out = rule.regex.replace_all(text, |caps: &Captures| {
        let token = &caps[0];
        if is_documentation_token(token) || (rule.kind == OPENAI && looks_like_kebab_slug(token)) {
            return token.to_string();
        }
        hits += 1;
        marker(rule.kind)
    });
    if hits == 0 {
        return None;
    }
    counts.0[rule.kind] += hits;
    Some(out.into_owned())
}

fn redact_jwt(text: &str, counts: &mut SecretCounts) -> Option<String> {
    let mut hits = 0usize;
    let out = JWT_RE.replace_all(text, |_: &Captures| {
        hits += 1;
        marker(JWT)
    });
    if hits == 0 {
        return None;
    }
    counts.0[JWT] += hits;
    Some(out.into_owned())
}

/// BEGIN line through the matching END line, or to the end of the text when
/// the END never arrives (a truncated tool result).
fn redact_private_keys(text: &str, counts: &mut SecretCounts) -> Option<String> {
    let mut out = String::new();
    let mut cursor = 0usize;
    let mut hits = 0usize;
    while let Some(begin) = PRIVATE_KEY_BEGIN.find_at(text, cursor) {
        let end = PRIVATE_KEY_END
            .find_at(text, begin.end())
            .map_or(text.len(), |end| end.end());
        out.push_str(&text[cursor..begin.start()]);
        out.push_str(&marker(PRIVATE_KEY));
        cursor = end;
        hits += 1;
    }
    if hits == 0 {
        return None;
    }
    out.push_str(&text[cursor..]);
    counts.0[PRIVATE_KEY] += hits;
    Some(out)
}

fn redact_env_secrets(text: &str, counts: &mut SecretCounts) -> Option<String> {
    let mut hits = 0usize;
    let out = ENV_SECRET_RE.replace_all(text, |caps: &Captures| {
        // (quote char, value) of whichever alternative matched.
        let (quote, value) = if let Some(value) = caps.name("dq") {
            ("\"", value.as_str())
        } else if let Some(value) = caps.name("sq") {
            ("'", value.as_str())
        } else {
            (&caps["q"], &caps["val"])
        };
        let quoted = caps.name("val").is_none();
        if (quoted && value.len() < 8) || is_placeholder_value(value) {
            return caps[0].to_string();
        }
        hits += 1;
        let closing = if quoted { quote } else { "" };
        format!("{}{quote}{}{closing}", &caps["pre"], marker(ENV_SECRET))
    });
    if hits == 0 {
        return None;
    }
    counts.0[ENV_SECRET] += hits;
    Some(out.into_owned())
}

/// Values that are not secrets: shell/template references, angle-bracket and
/// starred fillers, one repeated character, and the usual docs vocabulary.
fn is_placeholder_value(value: &str) -> bool {
    if value.contains(['$', '{', '<', '*']) || value.starts_with("[secret") {
        return true;
    }
    let mut chars = value.chars();
    if let Some(first) = chars.next() {
        if chars.all(|c| c == first) {
            return true;
        }
    }
    let lower = value.to_ascii_lowercase();
    ["your", "example", "changeme", "xxxx", "placeholder"]
        .iter()
        .any(|word| lower.contains(word))
}

/// Vendor documentation keys (`AKIAIOSFODNN7EXAMPLE`) are not credentials.
fn is_documentation_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    ["example", "placeholder", "changeme"]
        .iter()
        .any(|word| lower.contains(word))
}

/// `sk-` plus 32 characters also matches a kebab-case slug such as
/// `task-queue-implementation-with-retries-and-backoff`. Real keys carry
/// digits or capitals; a lowercase, hyphenated run does not.
fn looks_like_kebab_slug(token: &str) -> bool {
    let body = token.strip_prefix("sk-").unwrap_or(token);
    body.contains('-')
        && !body
            .bytes()
            .any(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures are assembled at runtime so no literal credential-shaped string
    // sits in the source tree.
    fn rep(prefix: &str, ch: char, n: usize) -> String {
        format!("{prefix}{}", ch.to_string().repeat(n))
    }

    fn varied(n: usize) -> String {
        "aB3dE5gH7jK9mN1pQ3rS5tU7vW9xY1zC"
            .chars()
            .cycle()
            .take(n)
            .collect()
    }

    fn positives() -> Vec<(&'static str, String)> {
        let jwt = format!(
            "{}.{}.{}",
            rep("eyJ", 'h', 20),
            rep("eyJ", 'p', 20),
            "s".repeat(20)
        );
        vec![
            ("anthropic_key", format!("sk-ant-{}", varied(40))),
            ("openai_key", format!("sk-{}", varied(48))),
            ("openai_key", format!("sk-proj-{}", varied(48))),
            ("openrouter_key", format!("sk-or-v1-{}", "a1".repeat(24))),
            ("github_token", rep("ghp_", 'A', 36)),
            ("github_token", format!("github_pat_{}", varied(60))),
            ("aws_access_key", rep("AKIA", 'Q', 16)),
            ("aws_access_key", rep("ASIA", '7', 16)),
            ("slack_token", format!("xoxb-{}", "1234567890")),
            ("google_api_key", rep("AIza", 'k', 35)),
            ("stripe_key", rep("sk_live_", 'Z', 24)),
            ("stripe_key", rep("rk_live_", 'Z', 24)),
            ("npm_token", rep("npm_", 'n', 36)),
            ("jwt", jwt),
        ]
    }

    #[test]
    fn every_token_kind_is_redacted_and_counted() {
        for (kind, secret) in positives() {
            let text = format!("before {secret} after");
            let mut counts = SecretCounts::default();
            let out = redact_secrets_counted(&text, &mut counts);
            assert_eq!(
                out,
                format!("before [secret redacted: {kind}] after"),
                "{kind}"
            );
            assert_eq!(counts.get(kind), 1, "{kind}");
            assert_eq!(counts.total(), 1, "{kind}");
        }
    }

    #[test]
    fn env_secret_keeps_the_name_and_redacts_the_value() {
        let value = varied(24);
        let text = format!("MY_SERVICE_API_KEY={value}\nexport DB_PASSWORD={value}\nPLAIN=ok");
        let mut counts = SecretCounts::default();
        let out = redact_secrets_counted(&text, &mut counts);
        assert_eq!(
            out,
            "MY_SERVICE_API_KEY=[secret redacted: env_secret]\n\
             export DB_PASSWORD=[secret redacted: env_secret]\nPLAIN=ok"
        );
        assert_eq!(counts.get("env_secret"), 2);
    }

    #[test]
    fn env_secret_handles_quoted_values_and_indentation() {
        let value = varied(20);
        let text = format!("  AUTH_TOKEN=\"{value}\"");
        assert_eq!(
            redact_secrets(&text),
            "  AUTH_TOKEN=\"[secret redacted: env_secret]\""
        );
    }

    #[test]
    fn quoted_env_values_are_redacted_through_the_closing_quote() {
        // A passphrase with spaces: the whole value goes, not one word of it.
        assert_eq!(
            redact_secrets("PASSWORD=\"correct horse battery staple\""),
            "PASSWORD=\"[secret redacted: env_secret]\""
        );
        assert_eq!(
            redact_secrets("export DB_PASSWORD='elephants horse battery'\nnext line"),
            "export DB_PASSWORD='[secret redacted: env_secret]'\nnext line"
        );
        // Length and placeholder checks apply to the whole quoted value.
        for text in [
            "PASSWORD=\"hunter2\"",
            "PASSWORD=\"enter your password here\"",
            "PASSWORD=\"${DB_PASSWORD} and more\"",
        ] {
            assert!(matches!(redact_secrets(text), Cow::Borrowed(_)), "{text}");
        }
        // An unclosed quote still falls back to the first word.
        assert_eq!(
            redact_secrets("PASSWORD=\"elephants and more"),
            "PASSWORD=\"[secret redacted: env_secret] and more"
        );
        let once = redact_secrets("PASSWORD=\"correct horse battery staple\"").into_owned();
        assert!(matches!(redact_secrets(&once), Cow::Borrowed(_)));
    }

    #[test]
    fn env_secret_only_matches_at_line_start() {
        let value = varied(24);
        let text = format!("run it as MY_API_KEY={value} please");
        assert!(matches!(redact_secrets(&text), Cow::Borrowed(_)));
    }

    #[test]
    fn private_key_block_with_end_stops_at_the_end_line() {
        let body = varied(64);
        let text = format!(
            "key:\n-----BEGIN RSA PRIVATE KEY-----\n{body}\n-----END RSA PRIVATE KEY-----\ntail"
        );
        let mut counts = SecretCounts::default();
        let out = redact_secrets_counted(&text, &mut counts);
        assert_eq!(out, "key:\n[secret redacted: private_key_block]\ntail");
        assert_eq!(counts.get("private_key_block"), 1);
    }

    #[test]
    fn private_key_block_without_end_runs_to_the_end_of_text() {
        let body = varied(64);
        let text = format!("head\n-----BEGIN OPENSSH PRIVATE KEY-----\n{body}\ntruncated");
        assert_eq!(
            redact_secrets(&text),
            "head\n[secret redacted: private_key_block]"
        );
    }

    #[test]
    fn plain_private_key_header_and_two_blocks() {
        let text = "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\nmid\n\
                    -----BEGIN PGP PRIVATE KEY BLOCK-----\nxyz\n-----END PGP PRIVATE KEY BLOCK-----";
        let mut counts = SecretCounts::default();
        let out = redact_secrets_counted(text, &mut counts);
        assert_eq!(
            out,
            "[secret redacted: private_key_block]\nmid\n[secret redacted: private_key_block]"
        );
        assert_eq!(counts.get("private_key_block"), 2);
    }

    #[test]
    fn placeholders_and_short_values_are_left_alone() {
        let cases = [
            "API_KEY=your-api-key-here".to_string(),
            "SECRET_TOKEN=changeme-please".to_string(),
            "AUTH_TOKEN=$AUTH_TOKEN_VALUE".to_string(),
            "AUTH_TOKEN=${AUTH_TOKEN}".to_string(),
            "AUTH_TOKEN=<paste-token-here>".to_string(),
            "AUTH_TOKEN=********".to_string(),
            "AUTH_TOKEN=aaaaaaaaaaaa".to_string(),
            "DB_PASSWORD=example1234".to_string(),
            "DB_PASSWORD=xxxx1234abcd".to_string(),
            "DB_PASSWORD=placeholder99".to_string(),
            "DB_PASSWORD=short".to_string(),
            "AKIAIOSFODNN7EXAMPLE".to_string(),
            "set the API_KEY and the TOKEN before you run it".to_string(),
            "the SECRET is stored elsewhere".to_string(),
            "sk-ant-short".to_string(),
            "task-queue-implementation-with-retries-and-backoff".to_string(),
            "risk-adjusted-return-over-the-whole-portfolio-lifetime".to_string(),
            "sk-queue-implementation-with-retries-and-backoff".to_string(),
            rep("eyJ", 'a', 5),
            format!("ghp_{}", "A".repeat(10)),
        ];
        for text in cases {
            let mut counts = SecretCounts::default();
            let out = redact_secrets_counted(&text, &mut counts);
            assert!(matches!(out, Cow::Borrowed(_)), "{text}");
            assert!(counts.is_empty(), "{text}");
        }
    }

    #[test]
    fn no_match_returns_borrowed() {
        for text in [
            "",
            "short",
            "plain prose about nothing in particular, at all",
        ] {
            assert!(matches!(redact_secrets(text), Cow::Borrowed(_)), "{text}");
        }
        // A prefilter hit that no pattern confirms is also free.
        assert!(matches!(
            redact_secrets("the TOKEN budget and PASSWORD policy"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn redaction_is_idempotent() {
        let mut all = String::new();
        for (_, secret) in positives() {
            all.push_str(&format!("{secret}\n"));
        }
        all.push_str(&format!("DB_PASSWORD={}\n", varied(24)));
        all.push_str("-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n");
        all.push_str("-----BEGIN PRIVATE KEY-----\nunterminated");
        let once = redact_secrets(&all).into_owned();
        let mut counts = SecretCounts::default();
        let twice = redact_secrets_counted(&once, &mut counts);
        assert_eq!(twice, once);
        assert!(matches!(twice, Cow::Borrowed(_)));
        assert!(counts.is_empty());
    }

    #[test]
    fn unicode_text_survives_around_a_secret() {
        let secret = rep("ghp_", 'B', 36);
        let text = format!("héllo 世界 🔐 {secret} ünïcode ✓");
        assert_eq!(
            redact_secrets(&text),
            "héllo 世界 🔐 [secret redacted: github_token] ünïcode ✓"
        );
        let plain = "héllo 世界 🔐 nothing to see ✓ ünïcode";
        assert!(matches!(redact_secrets(plain), Cow::Borrowed(_)));
    }

    #[test]
    fn counts_accumulate_per_kind_across_calls() {
        let mut counts = SecretCounts::default();
        let github = rep("ghp_", 'C', 36);
        let aws = rep("AKIA", 'D', 16);
        redact_secrets_counted(&format!("{github} {github}"), &mut counts);
        redact_secrets_counted(&aws, &mut counts);
        assert_eq!(counts.get("github_token"), 2);
        assert_eq!(counts.get("aws_access_key"), 1);
        assert_eq!(counts.total(), 3);
    }

    #[test]
    fn counts_round_trip_through_json_and_keep_the_per_kind_maximum() {
        let mut counts = SecretCounts::default();
        redact_secrets_counted(&rep("ghp_", 'E', 36), &mut counts);
        let json = counts.to_json().unwrap();
        assert_eq!(SecretCounts::from_json(Some(&json)), counts);
        assert_eq!(SecretCounts::from_json(None), SecretCounts::default());
        assert_eq!(SecretCounts::from_json(Some("not json")).total(), 0);
        assert!(SecretCounts::default().to_json().is_none());

        let mut other = SecretCounts::default();
        redact_secrets_counted(&rep("AKIA", 'F', 16), &mut other);
        let merged = counts.max_with(&other);
        assert_eq!(merged.get("github_token"), 1);
        assert_eq!(merged.get("aws_access_key"), 1);
    }

    #[test]
    fn kill_switch_leaves_text_untouched() {
        let secret = rep("ghp_", 'G', 36);
        let _off = DisabledForTest::set(true);
        let mut counts = SecretCounts::default();
        let out = redact_secrets_counted(&secret, &mut counts);
        assert_eq!(out, secret);
        assert!(counts.is_empty());
    }

    #[test]
    fn marker_matches_no_pattern() {
        for index in 0..KINDS.len() {
            let text = format!("x {} y", marker(index));
            assert!(matches!(redact_secrets(&text), Cow::Borrowed(_)), "{index}");
        }
    }
}
