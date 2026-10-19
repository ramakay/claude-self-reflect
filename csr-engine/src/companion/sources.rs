use super::{
    num,
    paths::{folders, read_json, Paths},
};
use serde_json::{json, Value};

pub fn kept_status(paths: &Paths, sid: &str, now: f64) -> Value {
    let v = paths
        .kept_file("statusline", sid)
        .map(|p| read_json(&p))
        .unwrap_or(Value::Null);
    if v["fields"].is_object() && now - num(&v["at"]) < 86400. {
        v
    } else {
        Value::Null
    }
}
pub fn account_limits(paths: &Paths, now: f64) -> Value {
    type Hit = (std::time::SystemTime, u64, f64, Value);
    static CACHE: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Hit>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let Ok(mut cache) = CACHE.lock() else {
        return json!({});
    };
    let mut best: Option<(f64, Value)> = None;
    for path in folders(&paths.keep.join("statusline")) {
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
            let limits = &kept["fields"]["rate_limits"];
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
            && now - hit.2 < 86400.
            && best.as_ref().is_none_or(|(at, _)| hit.2 > *at)
        {
            best = Some((hit.2, hit.3.clone()));
        }
    }
    best.map(|(_, v)| v).unwrap_or(json!({}))
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
                std::time::Duration::from_millis(5),
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
