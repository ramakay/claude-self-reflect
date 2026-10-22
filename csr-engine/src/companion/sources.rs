use super::{
    array, num,
    paths::{folders, read_json, Paths},
};
use serde_json::{json, Value};

/// How old a kept usage file may be and still be read.
const KEPT_FOR: f64 = 86400.;

/// The masthead's status fields (`context_window`, `rate_limits`) from one usage file, which is
/// what `usage::measure` wrote: the mod's `context`, `rateLimits` and `cost`. A window it has no
/// figure for is left out, never zeroed.
pub fn usage_fields(usage: &Value) -> Value {
    let mut out = serde_json::Map::new();
    let context = &usage["context"];
    let size = context["window"]
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.);
    let tokens = context["tokens"]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.)
        .or_else(|| {
            let percent = context["percent"].as_f64().filter(|n| n.is_finite())?;
            Some((size? * percent / 100.).round())
        });
    if size.is_some() || tokens.is_some() {
        let mut window = serde_json::Map::new();
        if let Some(tokens) = tokens {
            window.insert("total_input_tokens".into(), json!(tokens));
        }
        if let Some(size) = size {
            window.insert("context_window_size".into(), json!(size));
        }
        out.insert("context_window".into(), Value::Object(window));
    }
    let mut limits = serde_json::Map::new();
    for row in array(&usage["rateLimits"]) {
        let (Some(kind), Some(used)) = (
            row["kind"].as_str().filter(|k| !k.is_empty()),
            row["percentUsed"].as_f64().filter(|n| n.is_finite()),
        ) else {
            continue;
        };
        let mut window = serde_json::Map::new();
        window.insert("used_percentage".into(), json!(used));
        if let Some(resets) = row["resetsAt"].as_str().and_then(super::paths::epoch) {
            window.insert("resets_at".into(), json!(resets));
        }
        limits.insert(kind.to_owned(), Value::Object(window));
    }
    out.insert("rate_limits".into(), Value::Object(limits));
    Value::Object(out)
}

/// One session's masthead fields: its usage file when a fresh one exists, plus `prompt_cache`
/// from the newest reply in its transcript. Null when there is neither.
pub fn kept_status(paths: &Paths, sid: &str, now: f64) -> Value {
    let usage = paths
        .kept_file(super::usage::FOLDER, sid)
        .map(|p| read_json(&p))
        .unwrap_or(Value::Null);
    let fresh = usage.is_object() && now - num(&usage["at"]) < KEPT_FOR;
    let mut fields = if fresh {
        usage_fields(&usage)
    } else {
        json!({})
    };
    let tokens = fields["context_window"]["total_input_tokens"]
        .as_f64()
        .filter(|n| *n > 0.);
    if let Some(cache) = prompt_cache(paths, sid, now, tokens) {
        fields["prompt_cache"] = cache;
    }
    if fields.as_object().is_some_and(|v| v.is_empty()) {
        return Value::Null;
    }
    json!({ "at": if fresh { usage["at"].clone() } else { Value::Null }, "fields": fields })
}

/// The account's rate-limit windows, from the newest usage file of any session.
pub fn account_limits(paths: &Paths, now: f64) -> Value {
    type Hit = (std::time::SystemTime, u64, f64, Value);
    static CACHE: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Hit>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let Ok(mut cache) = CACHE.lock() else {
        return json!({});
    };
    let mut best: Option<(f64, Value)> = None;
    for path in folders(&paths.keep.join(super::usage::FOLDER)) {
        if path.extension().is_none_or(|s| s != "json") {
            continue;
        }
        let Ok(stat) = std::fs::metadata(&path) else {
            continue;
        };
        let Ok(modified) = stat.modified() else {
            continue;
        };
        if cache
            .get(&path)
            .is_none_or(|hit| hit.0 != modified || hit.1 != stat.len())
        {
            let kept = read_json(&path);
            let limits = &usage_fields(&kept)["rate_limits"];
            cache.insert(
                path.clone(),
                (
                    modified,
                    stat.len(),
                    num(&kept["at"]),
                    if limits.is_object() {
                        limits.clone()
                    } else {
                        Value::Null
                    },
                ),
            );
        }
        let hit = &cache[&path];
        if hit.3.as_object().is_some_and(|v| !v.is_empty())
            && now - hit.2 < KEPT_FOR
            && best.as_ref().is_none_or(|(at, _)| hit.2 > *at)
        {
            best = Some((hit.2, hit.3.clone()));
        }
    }
    best.map(|(_, v)| v).unwrap_or(json!({}))
}

/// How long the prompt cache holds after a reply: an hour once the reply wrote 1 h entries.
const CACHE_TTL: f64 = 300.;
const CACHE_TTL_LONG: f64 = 3600.;

/// The newest main-thread reply of a transcript that reported usage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reply {
    pub at: f64,
    pub long: bool,
    pub tokens: f64,
}

/// The newest reply in `text`, a run of whole transcript lines. A reply with no input at all is
/// not one: it is an error notice, not a response.
pub fn newest_reply(text: &[u8]) -> Option<Reply> {
    text.rsplit(|b| *b == b'\n').find_map(|line| {
        if !contains(line, b"\"usage\"") || !contains(line, b"assistant") {
            return None;
        }
        let e = sonic_rs::from_slice::<Value>(line).ok()?;
        if e["type"] != "assistant" || e["isSidechain"] == true {
            return None;
        }
        let usage = &e["message"]["usage"];
        if !usage.is_object() {
            return None;
        }
        let tokens: f64 = [
            "input_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ]
        .iter()
        .map(|k| num(&usage[k]))
        .sum();
        let at = e["timestamp"].as_str().and_then(super::paths::epoch)?;
        (tokens > 0.).then(|| Reply {
            at,
            long: num(&usage["cache_creation"]["ephemeral_1h_input_tokens"]) > 0.,
            tokens,
        })
    })
}

fn contains(line: &[u8], needle: &[u8]) -> bool {
    line.windows(needle.len()).any(|w| w == needle)
}

/// The tail of `path` that holds a reply: 256 KiB first, up to 16 MiB for long tool results.
fn reply_in(path: &std::path::Path) -> Option<Reply> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut want = 256 * 1024_u64;
    loop {
        let from = len.saturating_sub(want);
        file.seek(SeekFrom::Start(from)).ok()?;
        let mut bytes = vec![];
        file.by_ref()
            .take(len - from)
            .read_to_end(&mut bytes)
            .ok()?;
        let whole = if from == 0 {
            &bytes[..]
        } else {
            let line = bytes.iter().position(|b| *b == b'\n');
            &bytes[line.map_or(bytes.len(), |i| i + 1)..]
        };
        if let Some(reply) = newest_reply(whole) {
            return Some(reply);
        }
        if from == 0 || want >= 16 * 1024 * 1024 {
            return None;
        }
        want *= 4;
    }
}

/// A transcript's newest reply, read again only when the file changed; the file is looked for
/// again every ten seconds until it is found.
fn transcript_reply(paths: &Paths, sid: &str, now: f64) -> Option<Reply> {
    type Seen = (
        f64,
        Option<std::path::PathBuf>,
        Option<(std::time::SystemTime, u64)>,
        Option<Reply>,
    );
    static CACHE: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<(std::path::PathBuf, String), Seen>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if !super::paths::valid_id(sid) {
        return None;
    }
    let mut cache = CACHE.lock().ok()?;
    let seen = cache
        .entry((paths.home.clone(), sid.to_owned()))
        .or_insert((f64::MIN, None, None, None));
    if seen.1.as_ref().is_none_or(|p| !p.is_file()) {
        if now >= seen.0 && now - seen.0 < 10. {
            return None;
        }
        seen.0 = now;
        seen.1 = super::session::session_record(paths, sid)
            .map(|record| {
                paths
                    .project_folder(record["cwd"].as_str().unwrap_or(""))
                    .join(format!("{sid}.jsonl"))
            })
            .filter(|p| p.is_file())
            .or_else(|| paths.transcript(sid));
        seen.2 = None;
        seen.3 = None;
    }
    let path = seen.1.clone()?;
    let stat = std::fs::metadata(&path).ok()?;
    let mark = (stat.modified().ok()?, stat.len());
    if seen.2 != Some(mark) {
        seen.3 = reply_in(&path);
        seen.2 = Some(mark);
    }
    seen.3
}

/// The `prompt_cache` the masthead reads, from the newest reply of the session's transcript: the
/// cache holds for an hour after a reply that wrote 1 h entries, five minutes otherwise. `tokens`
/// is the context the usage file reports; the reply's own input stands in without it.
pub fn prompt_cache(paths: &Paths, sid: &str, now: f64, tokens: Option<f64>) -> Option<Value> {
    let reply = transcript_reply(paths, sid, now)?;
    let ttl = if reply.long {
        CACHE_TTL_LONG
    } else {
        CACHE_TTL
    };
    let expires = reply.at + ttl;
    Some(json!({
        "warm": now < expires,
        "ttl": if reply.long { "1h" } else { "5m" },
        "expires_at": expires,
        "recache_tokens_if_cold": tokens.unwrap_or(reply.tokens),
    }))
}

pub fn csr_status(paths: &Paths) -> Value {
    csr_status_at(paths, super::paths::now())
}
pub fn csr_status_at(paths: &Paths, now: f64) -> Value {
    type Cache = std::collections::HashMap<std::path::PathBuf, (f64, Value)>;
    static CACHE: std::sync::LazyLock<std::sync::Mutex<Cache>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(Cache::new()));
    if let Ok(cache) = CACHE.lock() {
        if let Some((at, value)) = cache.get(&paths.home) {
            if now >= *at && now - *at < 60. {
                return value.clone();
            }
        }
    }

    let lock = paths.home.join(".claude-self-reflect/daemon.lock");
    let pid = std::fs::read_to_string(lock)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .filter(|p| *p > 1);
    let up=pid.is_some_and(|pid|unsafe {
        libc::kill(pid,
        0)
    }==0||std::io::Error::last_os_error().raw_os_error()==Some(libc::EPERM));
    let mut inop = vec![];
    if !up {
        inop.push(json!([
            "DAEMON",
            "DOWN · no live pid in daemon.lock",
            "amber",
        ]));
    }
    let mut switches = vec![];
    let re =
        regex::Regex::new(r"<key>(CSR_NO_[A-Z0-9_]+)</key>\s*<string>([^<]*)</string>").unwrap();
    for p in folders(&paths.home.join("Library/LaunchAgents"))
        .iter()
        .filter(|p| {
            p.to_string_lossy().contains("csr-engine")
                && p.extension().is_some_and(|e| e == "plist")
        })
    {
        if let Ok(s) = std::fs::read_to_string(p) {
            if s.contains("<key>EnvironmentVariables</key>") {
                for c in re.captures_iter(&s) {
                    switches.push((c[1].to_owned(), c[2].to_owned(), "daemon"));
                }
            }
        }
    }
    let settings = read_json(&paths.home.join(".claude/settings.json"));
    if let Some(env) = settings["env"].as_object() {
        for (k, v) in env {
            switches.push((
                k.clone(),
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string()),
                "hooks",
            ));
        }
    }
    for (k, v, source) in switches {
        if k.starts_with("CSR_NO_")
            && k != "CSR_NO_RATIFICATION"
            && !matches!(v.trim().to_lowercase().as_str(), "" | "0" | "false")
        {
            inop.push(json!([
                &k[7..],
                format!(
                    "INHIBITED · {k}={} ({source})",
                    super::render::text::one_line(&v, 24)
                ),
                "advisory",
            ]));
        }
    }
    let result = json!({
        "sev": if !up {
            json!("amber")
        } else if inop.is_empty() {
            Value::Null
        } else {
            json!("advisory")
        },
        "inop": inop,
        "daemon": {
            "up": up,
            "pid": if up { pid } else { None },
            "since": if up {
                std::fs::metadata(paths.home.join(".claude-self-reflect/daemon.lock"))
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|t| t.as_secs_f64())
            } else {
                None
            },
        },
    });
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(paths.home.clone(), (now, result.clone()));
    }
    result
}

/// Wall-clock budget one pane frame spends scanning past compactions; a large corpus
/// finishes across several frames.
const COMPACTION_SCAN_BUDGET_MS: u64 = 5;
static COMPACTION_SCAN_BUDGET: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(COMPACTION_SCAN_BUDGET_MS);

/// Test seam: give each frame's compaction scan a budget no test host can exhaust, so a
/// small fixture's first frame shows the finished scan on any CPU speed or load. Production
/// never calls this and keeps the 5 ms budget.
#[doc(hidden)]
pub fn set_compaction_scan_budget(budget: std::time::Duration) {
    COMPACTION_SCAN_BUDGET.store(
        u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
}

pub fn compacting_info(paths: &Paths, sid: &str, now: f64, persist: bool) -> Value {
    let marker = paths
        .kept_file("compacting", sid)
        .map(|p| read_json(&p))
        .unwrap_or(Value::Null);
    let at = marker["at"]
        .as_f64()
        .or_else(|| marker["at"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.);
    let at = if at > 1e11 { at / 1000. } else { at };
    if at == 0. || !at.is_finite() || now - at > 600. {
        return Value::Null;
    }
    type Scans =
        std::collections::HashMap<std::path::PathBuf, super::compaction_scan::CompactionScan>;
    static SCANS: std::sync::LazyLock<std::sync::Mutex<Scans>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(Scans::new()));
    let usual = SCANS
        .lock()
        .map(|mut scans| {
            scans.entry(paths.home.clone()).or_default().step(
                paths,
                now,
                persist,
                super::tail::CHUNK,
                std::time::Duration::from_millis(
                    COMPACTION_SCAN_BUDGET.load(std::sync::atomic::Ordering::Relaxed),
                ),
                super::watch::stopping,
            )
        })
        .unwrap_or(Value::Null);
    json!({
        "elapsed": (now - at).max(0.).floor(),
        "trigger": marker["trigger"].as_str().unwrap_or(""),
        "usual": usual,
        "facts": [],
    })
}

pub fn threshold(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite())
        .unwrap_or(default)
        .max(1.)
}

pub fn picture_fonts() -> &'static super::render::raster::Fonts {
    use super::render::raster::Fonts;
    use ab_glyph::{FontArc, FontVec};
    static FONTS: std::sync::LazyLock<Fonts> = std::sync::LazyLock::new(|| {
        let load = |bold: bool| {
            [
                ("/System/Library/Fonts/Menlo.ttc", u32::from(bold)),
                ("/System/Library/Fonts/SFNSMono.ttf", 0),
                // Linux has no Menlo; the removed `pane.py` falls back to Pillow's own font there.
                if bold {
                    (
                        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
                        0,
                    )
                } else {
                    ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 0)
                },
            ]
            .into_iter()
            .find_map(|(path, index)| {
                let bytes = std::fs::read(path).ok()?;
                FontVec::try_from_vec_and_index(bytes, index)
                    .ok()
                    .map(FontArc::new)
            })
        };
        Fonts {
            regular: load(false),
            bold: load(true),
        }
    });
    &FONTS
}

pub fn mtime(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|t| t.as_nanos() as u64)
}
pub fn newest_transcripts(paths: &Paths, newest: usize) -> Vec<(u64, std::path::PathBuf)> {
    let mut found: Vec<_> = folders(&paths.projects)
        .iter()
        .flat_map(|p| folders(p))
        .filter(|p| p.extension().is_some_and(|s| s == "jsonl"))
        .map(|p| (mtime(&p).unwrap_or(0), p))
        .collect();
    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    found.truncate(newest);
    found
}

pub fn continued_from_at(
    paths: &Paths,
    sid: &str,
    given: Option<&std::path::Path>,
    now: f64,
) -> Option<String> {
    if !super::paths::valid_id(sid)
        || std::env::var("CSR_NO_CONTINUED")
            .is_ok_and(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true"))
    {
        return None;
    }
    type Seen = std::collections::HashMap<(std::path::PathBuf, String), (Option<String>, f64)>;
    static SEEN: std::sync::LazyLock<std::sync::Mutex<Seen>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(Seen::new()));
    let key = (paths.home.clone(), sid.to_owned());
    if let Ok(seen) = SEEN.lock() {
        if let Some((found, at)) = seen.get(&key) {
            if found.is_some() || now - at < 60. {
                return found.clone();
            }
        }
    }
    let mine = if let Some(given) = given {
        given.is_file().then(|| given.to_owned())
    } else {
        paths.transcript(sid)
    }?;
    let folder = mine.parent()?;
    std::fs::read_dir(folder).ok()?;
    let found = crate::hooks::resume_lineage::pane_predecessor(&mine, sid);
    if !mine.is_file() || std::fs::read_dir(folder).is_err() {
        return None;
    }
    if let Ok(mut seen) = SEEN.lock() {
        seen.insert(key, (found.clone(), now));
    }
    found
}

pub fn previous_session(
    paths: &Paths,
    mine: &std::path::Path,
    started: f64,
) -> Option<std::path::PathBuf> {
    if started == 0. || !mine.is_file() {
        return None;
    }
    let sid = mine.file_stem()?.to_str()?;
    if let Some(old) = continued_from_at(paths, sid, Some(mine), super::paths::now()) {
        let sibling = mine.parent()?.join(format!("{old}.jsonl"));
        if sibling.is_file() && paths.transcript(&old).is_some() {
            return Some(sibling);
        }
    }
    let mut candidates: Vec<_> = folders(mine.parent()?)
        .into_iter()
        .filter_map(|path| {
            if path == mine
                || path.extension().is_none_or(|e| e != "jsonl")
                || path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_none_or(|id| paths.transcript(id).is_none())
            {
                return None;
            }
            let at = mtime(&path)? as f64 / 1e9;
            (started - 7. * 86400. < at && at <= started).then_some((at, path))
        })
        .collect();
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
    let cli = regex::Regex::new(r#""entrypoint"\s*:\s*"cli""#).unwrap();
    candidates.into_iter().take(200).find_map(|(_, path)| {
        let head = super::tail::head(&path, 65536).ok()?;
        cli.is_match(&String::from_utf8_lossy(&head))
            .then_some(path)
    })
}

pub fn first_prompts(paths: &Paths) -> std::sync::Arc<std::collections::HashMap<String, String>> {
    type Titles = std::sync::Arc<std::collections::HashMap<String, String>>;
    type Cache = std::collections::HashMap<std::path::PathBuf, (u64, Titles)>;
    static CACHE: std::sync::LazyLock<std::sync::Mutex<Cache>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(Cache::new()));
    let path = paths.home.join(".claude/history.jsonl");
    let Some(modified) = mtime(&path) else {
        return Titles::default();
    };
    let Ok(mut cache) = CACHE.lock() else {
        return Titles::default();
    };
    if let Some((old, titles)) = cache.get(&path) {
        if *old == modified {
            return titles.clone();
        }
    }
    let mut titles = std::collections::HashMap::new();
    super::tail::scan(&path, |entry| {
        let sid = super::string(&entry["sessionId"]);
        let text = super::string(&entry["display"]).trim();
        if !sid.is_empty() && !text.is_empty() && !text.starts_with('/') {
            titles
                .entry(sid.to_owned())
                .or_insert_with(|| text.to_owned());
        }
    });
    let titles = std::sync::Arc::new(titles);
    cache.insert(path, (modified, titles.clone()));
    titles
}

pub fn commit_infos(
    root: &std::path::Path,
    oids: &[String],
) -> std::collections::BTreeMap<String, (bool, String)> {
    let ids: std::collections::BTreeSet<_> = oids
        .iter()
        .filter(|s| (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(String::as_str)
        .collect();
    let ids: Vec<_> = ids.into_iter().collect();
    let mut out = std::collections::BTreeMap::new();
    for chunk in ids.chunks(400) {
        let mut args = vec!["show", "-s", "--format=%H%x00%P%x00%s%x00"];
        args.extend_from_slice(chunk);
        let Some(text) = super::paths::git(root, &args) else {
            for id in chunk {
                out.insert((*id).to_owned(), commit_info(root, id));
            }
            continue;
        };
        let fields: Vec<_> = text.split('\0').collect();
        let records: Vec<_> = fields
            .as_chunks::<3>()
            .0
            .iter()
            .map(|r| {
                (
                    r[0].trim(),
                    r[1].split_whitespace().count() > 1,
                    r[2].trim().to_owned(),
                )
            })
            .collect();
        for id in chunk {
            let found = records.iter().find(|(full, _, _)| {
                full.get(..id.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(id))
            });
            out.insert(
                (*id).to_owned(),
                found
                    .map(|(_, merge, subject)| (*merge, subject.clone()))
                    .unwrap_or_default(),
            );
        }
    }
    out
}
fn commit_info(root: &std::path::Path, oid: &str) -> (bool, String) {
    let text = super::paths::git(root, &["show", "-s", "--format=%H%x00%P%x00%s", oid])
        .unwrap_or_default();
    let mut fields = text.split('\0');
    fields.next();
    (
        fields.next().unwrap_or("").split_whitespace().count() > 1,
        fields.next().unwrap_or("").to_owned(),
    )
}
