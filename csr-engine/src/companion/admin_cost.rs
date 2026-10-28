//! Real Anthropic spend for the API credit row, from the Admin cost API, and the state of the
//! admin key that makes it possible.
//!
//! # When to use the admin key
//!
//! * Read-only cost reporting only: `GET /v1/organizations/cost_report`, nothing else.
//! * It lives in the login Keychain (service `csr-anthropic-admin`) and nowhere else. It is read
//!   in this process by `/usr/bin/security` and handed to `curl` on stdin: never in argv, never
//!   in an environment, never in a file, a log, a prompt, a transcript or a worker's env.
//! * The cache (`~/.claude-self-reflect/api-cost.json`) holds dollar figures, times and the key's
//!   state. It never holds the key, nor any text the API sent back.
//! * If the key ever appears in any output, rotate it.
//!
//! One fetch per ten minutes across every pane: a file lock decides who fetches, the rest read
//! the cache. The pane only ever reads the cache; the fetch runs on its own thread.
//!
//! The state comes from what the API answers, never from the key's prefix: a `usr` key can be
//! organization-scoped and valid. Cost buckets are UTC days, and `credit.json`'s `since` is read
//! as a UTC date here, so a run late on a local evening (already tomorrow in UTC) counts.
use super::api_credit::{Credit, KeyNote};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const SERVICE: &str = "csr-anthropic-admin";
/// Seconds between fetches.
pub const TTL: f64 = 600.;
const ENDPOINT: &str = "https://api.anthropic.com/v1/organizations/cost_report";
const MAX_PAGES: usize = 12;

/// What the outside world gives: the Keychain and the network. Tests pass fakes.
pub trait Host {
    /// `security dump-keychain` (attributes only, no secrets).
    fn keychain_dump(&self) -> String;
    /// The password of the item with this account in this keychain (either may be empty).
    fn keychain_password(&self, acct: &str, keychain: &str) -> Option<String>;
    /// GET with the key as `x-api-key`: the status and the body.
    fn http_get(&self, key: &str, url: &str) -> Result<(u16, String), String>;
}

pub struct Real;

impl Host for Real {
    fn keychain_dump(&self) -> String {
        std::process::Command::new("/usr/bin/security")
            .arg("dump-keychain")
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }
    fn keychain_password(&self, acct: &str, keychain: &str) -> Option<String> {
        let mut cmd = std::process::Command::new("/usr/bin/security");
        cmd.args(["find-generic-password", "-s", SERVICE]);
        if !acct.is_empty() {
            cmd.args(["-a", acct]);
        }
        cmd.arg("-w");
        if !keychain.is_empty() {
            cmd.arg(keychain);
        }
        let out = cmd.stderr(std::process::Stdio::null()).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .filter(|k| !k.is_empty())
    }
    fn http_get(&self, key: &str, url: &str) -> Result<(u16, String), String> {
        use std::io::Write;
        let mut child = std::process::Command::new("curl")
            .args(["-s", "--connect-timeout", "8", "-m", "20"])
            .args(["-w", "\n%{http_code}", "-H", "@-"])
            .args(["-H", "anthropic-version: 2023-06-01"])
            .args(["-A", concat!("csr-engine/", env!("CARGO_PKG_VERSION"))])
            .arg(url)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| "curl did not start".to_owned())?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "x-api-key: {key}");
        }
        let out = child
            .wait_with_output()
            .map_err(|_| "curl failed".to_owned())?;
        let text = String::from_utf8_lossy(&out.stdout);
        let (body, code) = text.rsplit_once('\n').ok_or("no answer")?;
        match code.trim().parse::<u16>() {
            Ok(c) if c != 0 => Ok((c, body.to_owned())),
            _ => Err("no answer".into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum State {
    Connected,
    Missing,
    /// The key's type tag (`usr`, `admin`, ...), never the key.
    Rejected(String),
    Individual,
    Offline,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cache {
    pub attempted_at: f64,
    pub fetched_at: f64,
    /// First day (UTC, `YYYY-MM-DD`) the figures cover.
    pub from: String,
    pub state: State,
    /// Two keychain entries that differ.
    pub dup: bool,
    /// The last fetch failed on the network: these are the figures of `fetched_at`.
    pub stale: bool,
    /// Dollars by UTC day.
    pub days: BTreeMap<String, f64>,
}

/// `usr`, `admin`, ...: the letters between `sk-ant-` and the version digits. Anything else is
/// "unknown". The tag is all of a key the pane ever says.
pub fn tag_of(key: &str) -> String {
    let rest = key.strip_prefix("sk-ant-").unwrap_or("");
    let tag: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let digit_follows = rest[tag.len()..].starts_with(|c: char| c.is_ascii_digit());
    if (1..=12).contains(&tag.len()) && digit_follows {
        tag
    } else {
        "unknown".into()
    }
}

/// The (account, keychain) of every generic-password item of [`SERVICE`] in a
/// `security dump-keychain` listing.
pub fn keychain_items(dump: &str) -> Vec<(String, String)> {
    fn value(line: &str) -> Option<String> {
        let (_, v) = line.split_once("<blob>=")?;
        Some(v.trim().trim_matches('"').to_owned()).filter(|v| v != "<NULL>")
    }
    let mut out = Vec::new();
    let (mut keychain, mut acct, mut svce) = (String::new(), None::<String>, None::<String>);
    let mut flush = |keychain: &str, acct: &mut Option<String>, svce: &mut Option<String>| {
        if svce.take().as_deref() == Some(SERVICE) {
            out.push((acct.take().unwrap_or_default(), keychain.to_owned()));
        }
        *acct = None;
    };
    for line in dump.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("keychain:") {
            flush(&keychain, &mut acct, &mut svce);
            keychain = rest.trim().trim_matches('"').to_owned();
        } else if t.starts_with("version:") {
            flush(&keychain, &mut acct, &mut svce);
        } else if t.starts_with("\"acct\"<blob>=") {
            acct = value(t);
        } else if t.starts_with("\"svce\"<blob>=") {
            svce = value(t);
        }
    }
    flush(&keychain, &mut acct, &mut svce);
    out
}

/// Every distinct key stored under [`SERVICE`] (the admin one first), and whether there are two
/// that differ. Two copies of one key are not a difference.
pub fn read_keys(h: &dyn Host) -> (Vec<String>, bool) {
    let mut keys: Vec<String> = Vec::new();
    for (acct, keychain) in keychain_items(&h.keychain_dump()) {
        if let Some(k) = h.keychain_password(&acct, &keychain) {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    keys.sort_by_key(|k| tag_of(k) != "admin");
    let dup = keys.len() > 1;
    (keys, dup)
}

fn cents(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|c| c.is_finite())
}

/// An amount as the API gives it, USD cents in a decimal string, in dollars.
pub fn dollars(cents_text: &str) -> Option<f64> {
    cents(cents_text).map(|c| c / 100.)
}

/// One page of the cost report: dollars by UTC day, and the token of the next page.
pub fn parse_page(body: &str) -> Option<(BTreeMap<String, f64>, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut days = BTreeMap::new();
    for bucket in v["data"].as_array()? {
        let date = bucket["starting_at"].as_str()?.get(..10)?.to_owned();
        let mut total = 0.;
        for r in bucket["results"].as_array().into_iter().flatten() {
            total += match &r["amount"] {
                serde_json::Value::String(s) => cents(s).unwrap_or(0.),
                serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.),
                _ => 0.,
            };
        }
        *days.entry(date).or_insert(0.) += total / 100.;
    }
    let next = v["has_more"]
        .as_bool()
        .filter(|m| *m)
        .and_then(|_| v["next_page"].as_str())
        .map(str::to_owned);
    Some((days, next))
}

/// What a non-200 answer says about the key. The words of the body decide the individual-account
/// case; a 401/403 is a rejected key; anything else is the network's trouble.
pub fn classify_error(status: u16, body: &str, tag: &str) -> State {
    if body.to_ascii_lowercase().contains("individual account") {
        State::Individual
    } else if status == 401 || status == 403 {
        State::Rejected(tag.to_owned())
    } else {
        State::Offline
    }
}

/// `credit.json`'s `since` as a UTC date: a date, a naive date-time (its date), or an
/// offset date-time (its UTC date).
pub fn since_date(text: &str) -> Option<NaiveDate> {
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Some(t.with_timezone(&Utc).date_naive());
    }
    NaiveDate::parse_from_str(text.get(..10)?, "%Y-%m-%d").ok()
}

fn day_of(now: f64) -> NaiveDate {
    DateTime::<Utc>::from_timestamp(now as i64, 0)
        .map(|t| t.date_naive())
        .unwrap_or_default()
}

/// The first day to fetch: `since`, or six days back when that is earlier (the pace needs seven).
pub fn wanted_from(since: NaiveDate, now: f64) -> String {
    since
        .min(day_of(now) - Duration::days(6))
        .format("%Y-%m-%d")
        .to_string()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Ask for the cost report from `from` to the end of today (UTC), page by page.
pub fn fetch(h: &dyn Host, from: &str, now: f64, prev: Option<&Cache>) -> Cache {
    let (keys, dup) = read_keys(h);
    let base = |state: State| Cache {
        attempted_at: now,
        fetched_at: now,
        from: from.to_owned(),
        state,
        dup,
        stale: false,
        days: BTreeMap::new(),
    };
    // The network failed: keep what an earlier fetch found, if it reaches back far enough.
    let offline = || match prev {
        Some(p) if p.state == State::Connected && p.from.as_str() <= from => Cache {
            attempted_at: now,
            dup,
            stale: true,
            ..p.clone()
        },
        _ => base(State::Offline),
    };
    let Some(key) = keys.first() else {
        return base(State::Missing);
    };
    let tag = tag_of(key);
    let to = (day_of(now) + Duration::days(1)).format("%Y-%m-%d");
    let mut days: BTreeMap<String, f64> = BTreeMap::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url =
            format!("{ENDPOINT}?starting_at={from}T00:00:00Z&ending_at={to}T00:00:00Z&limit=31");
        if let Some(t) = &token {
            url.push_str(&format!("&page={}", encode(t)));
        }
        match h.http_get(key, &url) {
            Err(_) => return offline(),
            Ok((200, body)) => {
                let Some((page, next)) = parse_page(&body) else {
                    return offline();
                };
                for (d, v) in page {
                    *days.entry(d).or_insert(0.) += v;
                }
                match next {
                    Some(n) => token = Some(n),
                    None => {
                        let mut c = base(State::Connected);
                        c.days = days;
                        return c;
                    }
                }
            }
            Ok((status, body)) => {
                return match classify_error(status, &body, &tag) {
                    State::Offline => offline(),
                    state => base(state),
                }
            }
        }
    }
    offline()
}

pub fn cache_path(home: &Path) -> PathBuf {
    home.join(".claude-self-reflect/api-cost.json")
}

pub fn load(path: &Path) -> Option<Cache> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Asked within [`TTL`], and for a start no earlier than the cache covers.
pub fn fresh(c: &Cache, from: &str, now: f64) -> bool {
    now >= c.attempted_at && now - c.attempted_at < TTL && c.from.as_str() <= from
}

fn write(path: &Path, c: &Cache) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(serde_json::to_string(c)?.as_bytes())?;
    drop(f);
    std::fs::rename(&tmp, path)
}

/// Fetch and cache, unless the cache is fresh or another pane holds the lock. True when this call
/// fetched.
pub fn refresh(h: &dyn Host, home: &Path, from: &str, now: f64) -> bool {
    let path = cache_path(home);
    let Some(dir) = path.parent() else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let Ok(lock) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
    else {
        return false;
    };
    if lock.try_lock_exclusive().is_err() {
        return false;
    }
    let prev = load(&path);
    if prev.as_ref().is_some_and(|c| fresh(c, from, now)) {
        return false;
    }
    let c = fetch(h, from, now, prev.as_ref());
    let wrote = write(&path, &c).is_ok();
    let _ = FileExt::unlock(&lock);
    wrote
}

/// Off in this crate's own unit tests, and with `CSR_ADMIN_COST=off`.
pub fn enabled() -> bool {
    !cfg!(test) && std::env::var("CSR_ADMIN_COST").map_or(true, |v| v != "off" && v != "0")
}

static BUSY: AtomicBool = AtomicBool::new(false);

/// [`refresh`] with the real Keychain and network, on its own thread, one at a time.
pub fn spawn_refresh(home: PathBuf, from: String, now: f64) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("admin-cost".into())
        .spawn(move || {
            struct Done;
            impl Drop for Done {
                fn drop(&mut self) {
                    BUSY.store(false, Ordering::SeqCst);
                }
            }
            let _done = Done;
            refresh(&Real, &home, &from, now);
        });
    if spawned.is_err() {
        BUSY.store(false, Ordering::SeqCst);
    }
}

/// The api-lanes ledger's spend by UTC day (`YYYY-MM-DD`): rows with no `runner` or `runner ==
/// "claude"` only; Pi rows bill another provider.
pub fn ledger_by_day(text: &str) -> BTreeMap<String, f64> {
    let mut days = BTreeMap::new();
    for line in text.lines() {
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if row["runner"].as_str().is_some_and(|r| r != "claude") {
            continue;
        }
        let (Some(ts), Some(cost)) = (row["ts"].as_f64(), row["cost"].as_f64()) else {
            continue;
        };
        *days
            .entry(day_of(ts).format("%Y-%m-%d").to_string())
            .or_insert(0.) += cost;
    }
    days
}

/// Put the cache's real spend into `credit`: `left` is `amount` less the spend since `since`
/// (UTC days), `week` the last seven UTC days; and say the key's state. With no cache yet the
/// credit stays the ledger's.
pub fn apply(
    credit: &mut Credit,
    c: Option<&Cache>,
    amount: f64,
    since: NaiveDate,
    now: f64,
    ledger: &BTreeMap<String, f64>,
) {
    let Some(c) = c else {
        return;
    };
    credit.dup = c.dup;
    credit.stale = None;
    credit.ledger_added.clear();
    credit.key = match &c.state {
        State::Missing => KeyNote::Missing,
        State::Rejected(tag) => KeyNote::Rejected(tag.clone()),
        State::Individual => KeyNote::Individual,
        State::Offline => KeyNote::Offline,
        State::Connected if c.from.as_str() > wanted_from(since, now).as_str() => KeyNote::Offline,
        State::Connected => {
            let (since_s, week_s) = (
                since.format("%Y-%m-%d").to_string(),
                (day_of(now) - Duration::days(6))
                    .format("%Y-%m-%d")
                    .to_string(),
            );
            let sum = |from: &str| -> f64 {
                c.days
                    .range::<str, _>((std::ops::Bound::Included(from), std::ops::Bound::Unbounded))
                    .map(|d| d.1)
                    .sum()
            };
            // The API publishes a day late: for each UTC day from `since` to today it did not
            // return, the ledger's Claude spend stands in. A day it did return is the API's alone.
            let today = day_of(now).format("%Y-%m-%d").to_string();
            let (mut left, mut week) = (sum(&since_s), sum(&week_s));
            credit.ledger_added.clear();
            for (day, cost) in ledger.range::<str, _>((
                std::ops::Bound::Included(since_s.as_str()),
                std::ops::Bound::Included(today.as_str()),
            )) {
                if c.days.contains_key(day) || *cost <= 0. {
                    continue;
                }
                left += cost;
                if day.as_str() >= week_s.as_str() {
                    week += cost;
                }
                let label = if *day == today {
                    "today".to_owned()
                } else {
                    format!("on {}", day.get(5..).unwrap_or(day))
                };
                credit.ledger_added.push((label, *cost));
            }
            credit.left = amount - left;
            credit.week = week;
            credit.stale = c.stale.then(|| (now - c.fetched_at).max(0.));
            KeyNote::Connected
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    const KEY_USR: &str = "sk-ant-usr01-SECRETSECRETSECRET";
    const KEY_ADMIN: &str = "sk-ant-admin01-TOPSECRETTOPSECRET";
    /// The two real answers of 2026-10-28 and the individual-account one.
    const BODY_401: &str = r#"{"type":"error","error":{"type":"authentication_error","message":"The Admin API requires an Admin API key or an organization-scoped API key."}}"#;
    const BODY_INDIVIDUAL: &str = r#"{"type":"error","error":{"type":"permission_error","message":"The Admin API is unavailable for individual accounts."}}"#;

    fn at(y: i32, m: u32, d: u32, h: u32) -> f64 {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap().timestamp() as f64
    }
    fn now() -> f64 {
        at(2026, 10, 28, 10)
    }
    fn none() -> BTreeMap<String, f64> {
        BTreeMap::new()
    }
    fn dump(accts: &[&str]) -> String {
        let mut out = String::from("keychain: \"/Users/x/Library/Keychains/login.keychain-db\"\nversion: 512\nclass: \"inet\"\nattributes:\n    \"acct\"<blob>=\"other\"\n    \"svce\"<blob>=<NULL>\n");
        for a in accts {
            out += &format!("version: 512\nclass: \"genp\"\nattributes:\n    0x00000007 <blob>=\"{SERVICE}\"\n    \"acct\"<blob>=\"{a}\"\n    \"svce\"<blob>=\"{SERVICE}\"\n");
        }
        out += "version: 512\nclass: \"genp\"\nattributes:\n    \"acct\"<blob>=\"rama\"\n    \"svce\"<blob>=\"unrelated-service\"\n";
        out
    }
    fn page(days: &[(&str, &str)], next: Option<&str>) -> String {
        let data: Vec<_> = days
            .iter()
            .map(|(d, c)| {
                serde_json::json!({"starting_at": format!("{d}T00:00:00Z"),
                "ending_at": "x", "results": [{"amount": c, "currency": "USD"}, {"amount": "0.0", "currency": "USD"}]})
            })
            .collect();
        serde_json::json!({"data": data, "has_more": next.is_some(), "next_page": next}).to_string()
    }
    #[derive(Default)]
    struct Fake {
        dump: String,
        keys: Vec<(String, String)>,
        http: RefCell<VecDeque<Result<(u16, String), String>>>,
        urls: RefCell<Vec<String>>,
        used: RefCell<Vec<String>>,
    }
    impl Fake {
        fn with(accts: &[(&str, &str)], answers: Vec<Result<(u16, String), String>>) -> Fake {
            Fake {
                dump: dump(&accts.iter().map(|a| a.0).collect::<Vec<_>>()),
                keys: accts.iter().map(|a| (a.0.into(), a.1.into())).collect(),
                http: RefCell::new(answers.into()),
                ..Default::default()
            }
        }
    }
    impl Host for Fake {
        fn keychain_dump(&self) -> String {
            self.dump.clone()
        }
        fn keychain_password(&self, acct: &str, keychain: &str) -> Option<String> {
            assert!(keychain.ends_with("login.keychain-db"));
            self.keys.iter().find(|k| k.0 == acct).map(|k| k.1.clone())
        }
        fn http_get(&self, key: &str, url: &str) -> Result<(u16, String), String> {
            self.used.borrow_mut().push(key.into());
            self.urls.borrow_mut().push(url.into());
            self.http
                .borrow_mut()
                .pop_front()
                .unwrap_or(Err("no more".into()))
        }
    }
    fn ok(body: String) -> Result<(u16, String), String> {
        Ok((200, body))
    }

    #[test]
    fn the_tag_is_the_key_type_and_never_the_key() {
        assert_eq!(tag_of(KEY_USR), "usr");
        assert_eq!(tag_of(KEY_ADMIN), "admin");
        assert_eq!(tag_of("not-a-key-at-all"), "unknown");
        assert_eq!(tag_of(""), "unknown");
        assert_eq!(tag_of("sk-ant-averyveryveryverylongtag01-x"), "unknown");
    }

    #[test]
    fn cents_are_decimal_strings_and_become_dollars() {
        assert_eq!(dollars("16815.0000"), Some(168.15));
        assert_eq!(dollars("0"), Some(0.));
        assert_eq!(dollars(" 250.5 "), Some(2.505));
        assert_eq!(dollars("abc"), None);
        assert_eq!(dollars("NaN"), None);
    }

    #[test]
    fn a_page_sums_each_days_results_and_names_the_next_page() {
        let body = page(
            &[("2026-10-16", "10000.5"), ("2026-10-17", "6815")],
            Some("tok=/1"),
        );
        let (days, next) = parse_page(&body).unwrap();
        assert_eq!(days["2026-10-16"], 100.005);
        assert_eq!(days["2026-10-17"], 68.15);
        assert_eq!(next.as_deref(), Some("tok=/1"));
        let (_, none) = parse_page(&page(&[("2026-10-16", "1")], None)).unwrap();
        assert_eq!(none, None);
        assert!(parse_page("{").is_none());
        assert!(parse_page("{\"type\":\"error\"}").is_none());
    }

    #[test]
    fn the_keychain_dump_lists_every_item_of_the_service() {
        let items = keychain_items(&dump(&["rama", "ramakrishnan"]));
        assert_eq!(
            items,
            vec![
                (
                    "rama".into(),
                    "/Users/x/Library/Keychains/login.keychain-db".into()
                ),
                (
                    "ramakrishnan".into(),
                    "/Users/x/Library/Keychains/login.keychain-db".into()
                ),
            ]
        );
        assert!(keychain_items("").is_empty());
        assert!(keychain_items(&dump(&[])).is_empty());
    }

    #[test]
    fn two_entries_that_differ_are_flagged_and_the_admin_one_is_first() {
        let h = Fake::with(&[("a", KEY_USR), ("b", KEY_ADMIN)], vec![]);
        let (keys, dup) = read_keys(&h);
        assert_eq!(keys, vec![KEY_ADMIN.to_owned(), KEY_USR.to_owned()]);
        assert!(dup);
        let same = Fake::with(&[("a", KEY_USR), ("b", KEY_USR)], vec![]);
        let (keys, dup) = read_keys(&same);
        assert_eq!(keys, vec![KEY_USR.to_owned()]);
        assert!(!dup, "two copies of one key are not a difference");
        let none = Fake::with(&[], vec![]);
        assert_eq!(read_keys(&none), (vec![], false));
    }

    #[test]
    fn the_real_answers_map_to_key_states() {
        assert_eq!(
            classify_error(401, BODY_401, "usr"),
            State::Rejected("usr".into())
        );
        assert_eq!(
            classify_error(403, BODY_401, "admin"),
            State::Rejected("admin".into())
        );
        assert_eq!(
            classify_error(403, BODY_INDIVIDUAL, "usr"),
            State::Individual
        );
        assert_eq!(
            classify_error(400, BODY_INDIVIDUAL, "usr"),
            State::Individual
        );
        assert_eq!(classify_error(500, "oops", "usr"), State::Offline);
        assert_eq!(classify_error(429, "{}", "usr"), State::Offline);
    }

    #[test]
    fn each_key_state_comes_out_of_a_fetch() {
        let n = now();
        // Missing: nothing in the keychain, and no request is made.
        let h = Fake::with(&[], vec![]);
        let c = fetch(&h, "2026-10-27", n, None);
        assert_eq!(c.state, State::Missing);
        assert!(h.urls.borrow().is_empty());
        // Rejected: the real 401.
        let h = Fake::with(&[("a", KEY_USR)], vec![Ok((401, BODY_401.into()))]);
        assert_eq!(
            fetch(&h, "2026-10-27", n, None).state,
            State::Rejected("usr".into())
        );
        // Individual.
        let h = Fake::with(&[("a", KEY_USR)], vec![Ok((403, BODY_INDIVIDUAL.into()))]);
        assert_eq!(fetch(&h, "2026-10-27", n, None).state, State::Individual);
        // Connected.
        let h = Fake::with(
            &[("a", KEY_USR)],
            vec![ok(page(&[("2026-10-28", "1180")], None))],
        );
        let c = fetch(&h, "2026-10-27", n, None);
        assert_eq!(c.state, State::Connected);
        assert_eq!(c.days["2026-10-28"], 11.8);
        assert!(!c.dup && !c.stale);
        // The window: from the day asked to the end of today, UTC.
        let url = h.urls.borrow()[0].clone();
        assert!(url.starts_with(ENDPOINT), "{url}");
        assert!(url.contains("starting_at=2026-10-27T00:00:00Z"), "{url}");
        assert!(url.contains("ending_at=2026-10-29T00:00:00Z"), "{url}");
    }

    #[test]
    fn pages_are_followed_until_there_is_no_next_page() {
        let h = Fake::with(
            &[("a", KEY_USR)],
            vec![
                ok(page(&[("2026-10-27", "100")], Some("pg+2/="))),
                ok(page(&[("2026-10-28", "250")], None)),
            ],
        );
        let c = fetch(&h, "2026-10-27", now(), None);
        assert_eq!(c.days.len(), 2);
        assert_eq!(c.days["2026-10-28"], 2.5);
        let urls = h.urls.borrow();
        assert_eq!(urls.len(), 2);
        assert!(!urls[0].contains("page="));
        assert!(urls[1].ends_with("&page=pg%2B2%2F%3D"), "{}", urls[1]);
    }

    #[test]
    fn the_admin_key_is_the_one_used_when_two_differ() {
        let h = Fake::with(
            &[("a", KEY_USR), ("b", KEY_ADMIN)],
            vec![ok(page(&[], None))],
        );
        let c = fetch(&h, "2026-10-27", now(), None);
        assert_eq!(*h.used.borrow(), vec![KEY_ADMIN.to_owned()]);
        assert!(c.dup);
    }

    #[test]
    fn a_network_error_keeps_the_last_figures_and_says_how_old_they_are() {
        let n = now();
        let h = Fake::with(
            &[("a", KEY_USR)],
            vec![ok(page(&[("2026-10-28", "1180")], None))],
        );
        let good = fetch(&h, "2026-10-27", n, None);
        let h = Fake::with(&[("a", KEY_USR)], vec![Err("boom".into())]);
        let later = fetch(&h, "2026-10-27", n + 3600., Some(&good));
        assert_eq!(later.state, State::Connected);
        assert!(later.stale);
        assert_eq!(later.fetched_at, n);
        assert_eq!(later.attempted_at, n + 3600.);
        assert_eq!(later.days, good.days);
        // A 503 half way through the pages is the same.
        let h = Fake::with(
            &[("a", KEY_USR)],
            vec![
                ok(page(&[("2026-10-27", "5")], Some("p2"))),
                Ok((503, "x".into())),
            ],
        );
        let c = fetch(&h, "2026-10-27", n + 60., Some(&good));
        assert!(c.stale && c.days == good.days);
        // With nothing before it, there is nothing to keep.
        let h = Fake::with(&[("a", KEY_USR)], vec![Err("boom".into())]);
        assert_eq!(fetch(&h, "2026-10-27", n, None).state, State::Offline);
        // Figures that start later than the day asked are not kept.
        let h = Fake::with(&[("a", KEY_USR)], vec![Err("boom".into())]);
        assert_eq!(
            fetch(&h, "2026-10-01", n, Some(&good)).state,
            State::Offline
        );
    }

    #[test]
    fn one_fetch_per_ten_minutes_whoever_asks_and_the_lock_holder_alone_fetches() {
        let home = tempfile::tempdir().unwrap();
        let n = now();
        let answer = || ok(page(&[("2026-10-28", "1180")], None));
        let h = Fake::with(&[("a", KEY_USR)], vec![answer(), answer(), answer()]);
        assert!(refresh(&h, home.path(), "2026-10-27", n));
        let path = cache_path(home.path());
        assert_eq!(load(&path).unwrap().state, State::Connected);
        // Inside the ten minutes: served from the cache.
        assert!(!refresh(&h, home.path(), "2026-10-27", n + 599.));
        assert_eq!(h.urls.borrow().len(), 1);
        // After: fetched again.
        assert!(refresh(&h, home.path(), "2026-10-27", n + 601.));
        assert_eq!(h.urls.borrow().len(), 2);
        // A different pane holds the lock: no fetch, no wait.
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        assert!(!refresh(&h, home.path(), "2026-10-27", n + 5000.));
        assert_eq!(h.urls.borrow().len(), 2);
        drop(lock);
        // An earlier base date than the cache covers is a new question.
        assert!(refresh(&h, home.path(), "2026-10-01", n + 602.));
        assert_eq!(h.urls.borrow().len(), 3);
    }

    #[test]
    fn fresh_means_asked_within_ten_minutes_for_the_same_or_a_later_start() {
        let c = Cache {
            attempted_at: 1000.,
            fetched_at: 1000.,
            from: "2026-10-20".into(),
            state: State::Connected,
            dup: false,
            stale: false,
            days: BTreeMap::new(),
        };
        assert!(fresh(&c, "2026-10-20", 1100.));
        assert!(fresh(&c, "2026-10-25", 1599.));
        assert!(!fresh(&c, "2026-10-20", 1600.));
        assert!(!fresh(&c, "2026-10-19", 1100.));
        assert!(!fresh(&c, "2026-10-20", 900.), "a clock gone back is stale");
    }

    #[test]
    fn neither_the_cache_file_nor_any_row_ever_holds_the_key() {
        let home = tempfile::tempdir().unwrap();
        let n = now();
        type Answers = Vec<Result<(u16, String), String>>;
        let cases: Vec<(Vec<(&str, &str)>, Answers)> = vec![
            (vec![], vec![]),
            (
                vec![("a", KEY_USR)],
                vec![Ok((401, format!("{BODY_401} {KEY_USR}")))],
            ),
            (
                vec![("a", KEY_USR)],
                vec![Ok((403, BODY_INDIVIDUAL.into()))],
            ),
            (
                vec![("a", KEY_USR)],
                vec![Err(format!("failed with {KEY_USR}"))],
            ),
            (
                vec![("a", KEY_USR), ("b", KEY_ADMIN)],
                vec![ok(page(&[("2026-10-28", "1180")], None))],
            ),
        ];
        for (items, answers) in cases {
            let h = Fake::with(&items, answers);
            let _ = std::fs::remove_file(cache_path(home.path()));
            refresh(&h, home.path(), "2026-10-27", n);
            let text = std::fs::read_to_string(cache_path(home.path())).unwrap();
            assert!(
                !text.contains("sk-ant-") && !text.contains("SECRET"),
                "{text}"
            );
            let mut credit = Credit {
                left: 100.,
                ..Default::default()
            };
            let since = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
            apply(
                &mut credit,
                load(&cache_path(home.path())).as_ref(),
                243.56,
                since,
                n,
                &none(),
            );
            let row = credit.row();
            assert!(!row.contains("sk-ant-") && !row.contains("SECRET"), "{row}");
            assert!(row.starts_with("API credit $"), "{row}");
        }
    }

    #[test]
    fn the_cache_file_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let h = Fake::with(&[], vec![]);
        refresh(&h, home.path(), "2026-10-27", now());
        let mode = std::fs::metadata(cache_path(home.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }

    fn cache_of(days: &[(&str, f64)]) -> Cache {
        Cache {
            attempted_at: now(),
            fetched_at: now(),
            from: "2026-10-22".into(),
            state: State::Connected,
            dup: false,
            stale: false,
            days: days.iter().map(|d| (d.0.to_owned(), d.1)).collect(),
        }
    }
    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn left_is_the_base_less_real_spend_since_and_the_pace_is_the_last_seven_days() {
        let mut credit = Credit {
            left: 7.,
            week: 7.,
            ..Default::default()
        };
        let c = cache_of(&[
            ("2026-10-16", 100.),
            ("2026-10-17", 68.15),
            ("2026-10-23", 7.),
            ("2026-10-28", 11.8),
        ]);
        apply(
            &mut credit,
            Some(&c),
            243.56,
            d(2026, 10, 27),
            now(),
            &none(),
        );
        assert_eq!(credit.key, KeyNote::Connected);
        assert!((credit.left - 231.76).abs() < 1e-9, "{}", credit.left);
        // Seven UTC days ending today: 10-22 .. 10-28.
        assert!((credit.week - 18.8).abs() < 1e-9, "{}", credit.week);
        assert!((credit.per_day() - 18.8 / 7.).abs() < 1e-9);
        assert!(credit.days().unwrap() > 80.);
        assert_eq!(credit.stale, None);
    }

    #[test]
    fn a_run_between_local_and_utc_midnight_counts_under_either_since() {
        // 18:08 PDT on 10-27 is 01:08 UTC on 10-28: Anthropic puts its $11.80 in the 10-28 bucket.
        let c = cache_of(&[("2026-10-28", 11.8)]);
        for since in [d(2026, 10, 27), d(2026, 10, 28)] {
            let mut credit = Credit::default();
            apply(&mut credit, Some(&c), 243.56, since, now(), &none());
            assert!(
                (credit.left - 231.76).abs() < 1e-9,
                "{since} {}",
                credit.left
            );
        }
        let mut credit = Credit::default();
        apply(
            &mut credit,
            Some(&c),
            243.56,
            d(2026, 10, 29),
            now() + 86400.,
            &none(),
        );
        assert_eq!(credit.left, 243.56);
    }

    #[test]
    fn since_is_a_utc_date_from_a_date_a_time_or_an_offset_time() {
        assert_eq!(since_date("2026-10-27"), Some(d(2026, 10, 27)));
        assert_eq!(since_date("2026-10-27T18:08:00"), Some(d(2026, 10, 27)));
        assert_eq!(
            since_date("2026-10-27T18:08:00-07:00"),
            Some(d(2026, 10, 28))
        );
        assert_eq!(since_date("soon"), None);
        // The fetch starts at the earlier of since and six days back.
        assert_eq!(wanted_from(d(2026, 10, 27), now()), "2026-10-22");
        assert_eq!(wanted_from(d(2026, 10, 16), now()), "2026-10-16");
    }

    #[test]
    fn rows_say_each_key_state_in_words() {
        let n = now();
        let row = |state: State, dup: bool, stale: bool| {
            let mut c = cache_of(&[("2026-10-28", 11.8)]);
            c.state = state;
            c.dup = dup;
            c.stale = stale;
            if stale {
                c.fetched_at = n - 7200.;
            }
            let mut credit = Credit {
                left: 1.,
                ..Default::default()
            };
            apply(&mut credit, Some(&c), 243.56, d(2026, 10, 27), n, &none());
            credit.row()
        };
        assert_eq!(
            row(State::Connected, false, false),
            "API credit $231.76 left \u{b7} real Anthropic spend (admin key) \u{b7} about 137 days at this week's pace ($1.69/day)"
        );
        assert_eq!(
            row(State::Missing, false, false),
            "API credit $1.00 left (ledger estimate) \u{b7} for real spend, tell Claude: connect my Anthropic admin key"
        );
        assert_eq!(
            row(State::Rejected("usr".into()), false, false),
            "API credit $1.00 left (ledger estimate) \u{b7} Admin key rejected (usr key) \u{b7} needs an admin or org-scoped key \u{b7} tell Claude: fix my admin key"
        );
        assert_eq!(
            row(State::Individual, false, false),
            "API credit $1.00 left (ledger estimate) \u{b7} Admin API not available on this account \u{b7} using the ledger"
        );
        assert_eq!(
            row(State::Offline, false, false),
            "API credit $1.00 left (ledger estimate) \u{b7} Anthropic cost API unreachable"
        );
        assert!(row(State::Connected, false, true).ends_with("\u{b7} spend as of 2 h ago"));
        assert!(row(State::Missing, true, false)
            .ends_with("\u{b7} 2 keychain entries; using the admin one"));
        assert!(row(State::Connected, true, false)
            .ends_with("\u{b7} 2 keychain entries; using the admin one"));
    }

    #[test]
    fn no_cache_yet_leaves_the_ledger_row_alone() {
        let mut credit = Credit {
            left: 29.23,
            week: 94.63,
            ..Default::default()
        };
        let before = credit.clone();
        apply(&mut credit, None, 200., d(2026, 10, 1), now(), &none());
        assert_eq!(credit, before);
        assert!(credit.row().starts_with("API credit $29.23 left by ledger"));
    }

    #[test]
    fn figures_that_start_after_the_day_asked_are_not_used_for_the_sum() {
        let mut c = cache_of(&[("2026-10-28", 11.8)]);
        c.from = "2026-10-27".into();
        let mut credit = Credit {
            left: 5.,
            ..Default::default()
        };
        apply(
            &mut credit,
            Some(&c),
            243.56,
            d(2026, 10, 16),
            now(),
            &none(),
        );
        assert_eq!(credit.key, KeyNote::Offline);
        assert_eq!(credit.left, 5.);
    }

    fn row_of(ts: f64, runner: Option<&str>, cost: f64) -> String {
        let mut v = serde_json::json!({"ts": ts, "run": "x", "cost": cost});
        if let Some(r) = runner {
            v["runner"] = r.into();
        }
        v.to_string()
    }
    fn ledger_of(rows: &[String]) -> BTreeMap<String, f64> {
        ledger_by_day(&rows.join("\n"))
    }
    fn credit_with(c: &Cache, since: NaiveDate, n: f64, rows: &[String]) -> Credit {
        let mut credit = Credit::default();
        apply(&mut credit, Some(c), 243.56, since, n, &ledger_of(rows));
        credit
    }

    #[test]
    fn a_day_the_api_has_not_returned_is_counted_from_the_ledger_and_the_row_says_so() {
        // 03:20Z on 10-28: the API has only 10-27 ($0.00); the ledger has $14.40 on 10-28 UTC.
        let n = at(2026, 10, 28, 3) + 1200.;
        let c = cache_of(&[("2026-10-27", 0.)]);
        let rows = [
            row_of(at(2026, 10, 28, 2), None, 14.4),
            row_of(at(2026, 10, 27, 20), Some("claude"), 0.),
        ];
        let credit = credit_with(&c, d(2026, 10, 27), n, &rows);
        assert!((credit.left - 229.16).abs() < 1e-9, "{}", credit.left);
        assert!((credit.week - 14.4).abs() < 1e-9, "{}", credit.week);
        assert_eq!(
            credit.row(),
            "API credit $229.16 left \u{b7} real Anthropic spend (admin key) + $14.40 today by ledger \u{b7} about 111 days at this week's pace ($2.06/day)"
        );
    }

    #[test]
    fn an_earlier_day_the_api_lacks_is_named_and_counted_too() {
        let n = at(2026, 10, 28, 3);
        let c = cache_of(&[("2026-10-26", 1.)]);
        let rows = [
            row_of(at(2026, 10, 27, 12), None, 3.),
            row_of(at(2026, 10, 28, 1), None, 2.),
            // Before `since`, and after today: never counted.
            row_of(at(2026, 10, 20, 12), None, 50.),
            row_of(at(2026, 10, 29, 12), None, 60.),
        ];
        let credit = credit_with(&c, d(2026, 10, 26), n, &rows);
        assert!(
            (credit.left - (243.56 - 6.)).abs() < 1e-9,
            "{}",
            credit.left
        );
        assert!(credit
            .row()
            .contains("real Anthropic spend (admin key) + $3.00 on 10-27 + $2.00 today by ledger"));
    }

    #[test]
    fn a_day_the_api_returned_does_not_get_the_ledger_figure_added_again() {
        let n = now();
        let rows = [row_of(at(2026, 10, 28, 1), None, 14.4)];
        for api in [11.8, 0.] {
            let c = cache_of(&[("2026-10-28", api)]);
            let credit = credit_with(&c, d(2026, 10, 27), n, &rows);
            assert!(
                (credit.left - (243.56 - api)).abs() < 1e-9,
                "{}",
                credit.left
            );
            assert!(credit.ledger_added.is_empty());
            assert!(!credit.row().contains("by ledger"), "{}", credit.row());
        }
    }

    #[test]
    fn pi_rows_bill_another_provider_and_are_not_counted() {
        let rows = [
            row_of(at(2026, 10, 28, 2), Some("pi"), 9.),
            row_of(at(2026, 10, 28, 2), Some("claude"), 1.5),
            row_of(at(2026, 10, 28, 3), None, 2.5),
            "not json".to_owned(),
        ];
        let by_day = ledger_of(&rows);
        assert_eq!(by_day.len(), 1);
        assert!((by_day["2026-10-28"] - 4.).abs() < 1e-9);
        let c = cache_of(&[("2026-10-27", 0.)]);
        let credit = credit_with(&c, d(2026, 10, 27), now(), &rows);
        assert!(
            (credit.left - (243.56 - 4.)).abs() < 1e-9,
            "{}",
            credit.left
        );
        assert!(credit.row().contains("+ $4.00 today by ledger"));
    }

    #[test]
    fn a_run_at_0108z_is_a_utc_midnight_case_the_ledger_puts_on_the_new_utc_day() {
        // 18:08 PDT on 10-27 is 01:08Z on 10-28. The API has not published 10-28 yet, and a
        // row a minute before midnight UTC still belongs to 10-27, which it has.
        let n = at(2026, 10, 28, 1) + 25. * 60.;
        let c = cache_of(&[("2026-10-27", 5.)]);
        let rows = [
            row_of(at(2026, 10, 28, 1) + 8. * 60., None, 11.8),
            row_of(at(2026, 10, 28, 0) - 1., None, 7.),
        ];
        assert_eq!(
            ledger_of(&rows).keys().collect::<Vec<_>>(),
            ["2026-10-27", "2026-10-28"]
        );
        for since in [d(2026, 10, 27), d(2026, 10, 28)] {
            let credit = credit_with(&c, since, n, &rows);
            let spent = if since == d(2026, 10, 27) {
                5. + 11.8
            } else {
                11.8
            };
            assert!(
                (credit.left - (243.56 - spent)).abs() < 1e-9,
                "{since} {}",
                credit.left
            );
            assert!(credit.row().contains("+ $11.80 today by ledger"));
        }
    }
}
