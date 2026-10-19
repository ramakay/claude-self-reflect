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

pub fn picture_fonts() -> &'static super::render::raster::Fonts {
    use super::render::raster::Fonts;
    use ab_glyph::{FontArc, FontVec};
    static FONTS: std::sync::LazyLock<Fonts> = std::sync::LazyLock::new(|| {
        let load = |bold: bool| {
            [
                ("/System/Library/Fonts/Menlo.ttc", u32::from(bold)),
                ("/System/Library/Fonts/SFNSMono.ttf", 0),
                // Linux has no Menlo; pane.py falls back to Pillow's own font there.
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
