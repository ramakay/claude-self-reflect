use super::{
    array,
    paths::{read_json, Paths},
    tail::{Tail, CHUNK},
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, ReadDir},
    path::PathBuf,
    time::{Duration, Instant, UNIX_EPOCH},
};

#[derive(serde::Deserialize)]
struct Boundary {
    subtype: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "compactMetadata")]
    metadata: Option<Metadata>,
}
#[derive(serde::Deserialize)]
struct Metadata {
    #[serde(rename = "durationMs")]
    duration_ms: Option<f64>,
    #[serde(rename = "preTokens")]
    pre_tokens: Option<f64>,
    trigger: Option<String>,
}
#[derive(Default)]
struct Durations {
    tail: Tail,
    seconds: Vec<f64>,
    own: Vec<(f64, f64)>,
    modified: u64,
}

#[derive(Default, serde::Serialize, serde::Deserialize, PartialEq)]
struct OwnCache {
    version: u8,
    files: BTreeMap<PathBuf, OwnFile>,
}
#[derive(serde::Serialize, serde::Deserialize, PartialEq)]
struct OwnFile {
    modified: u64,
    bytes: u64,
    times: Vec<(f64, f64)>,
}
#[derive(Default)]
pub struct CompactionScan {
    directories: Option<ReadDir>,
    files: Option<ReadDir>,
    found: Vec<(u64, PathBuf)>,
    todo: VecDeque<(u64, PathBuf)>,
    values: BTreeMap<PathBuf, Durations>,
    started: bool,
    complete: bool,
    at: f64,
    kept: Value,
    own_since: Option<f64>,
    excluded: Vec<f64>,
    own_kept: OwnCache,
}
impl CompactionScan {
    pub fn step(
        &mut self,
        paths: &Paths,
        now: f64,
        persist: bool,
        bytes: usize,
        duration: Duration,
        mut stop: impl FnMut() -> bool,
    ) -> Value {
        self.advance(
            paths,
            now,
            persist,
            bytes,
            Some(Instant::now() + duration),
            &mut stop,
        )
    }
    fn advance(
        &mut self,
        paths: &Paths,
        now: f64,
        persist: bool,
        bytes: usize,
        end: Option<Instant>,
        mut stop: impl FnMut() -> bool,
    ) -> Value {
        if self.complete && now - self.at < 300. {
            return self.usual();
        }
        if !self.started || self.complete {
            self.at = now;
            self.complete = false;
            self.started = true;
            self.found.clear();
            self.directories = fs::read_dir(&paths.projects).ok();
            if self.own_since.is_some() {
                self.own_kept = serde_json::from_value(read_json(
                    &paths.keep.join("own-compaction-times.json"),
                ))
                .unwrap_or_default();
                if self.own_kept.version != 1 {
                    self.own_kept = OwnCache::default();
                }
            }
            self.kept = if self.own_since.is_none() {
                read_json(&paths.keep.join("compaction-times.json"))
            } else {
                Value::Null
            };
        }
        while self.directories.is_some() && end.is_none_or(|end| Instant::now() < end) && !stop() {
            if let Some(files) = &mut self.files {
                if let Some(Ok(file)) = files.next() {
                    let path = file.path();
                    if path.extension().is_some_and(|s| s == "jsonl") {
                        if let Some(time) = file.metadata().ok().and_then(|m| m.modified().ok()) {
                            let time = time.duration_since(UNIX_EPOCH).unwrap_or_default();
                            if self
                                .own_since
                                .is_some_and(|since| time.as_secs_f64() < since)
                            {
                                continue;
                            }
                            self.found.push((time.as_nanos() as u64, path));
                            if self.own_since.is_none() {
                                self.found.sort_by(|a, b| b.cmp(a));
                                self.found.truncate(60);
                            }
                        }
                    }
                    continue;
                }
                self.files = None;
            }
            match self.directories.as_mut().unwrap().next() {
                Some(Ok(folder)) => self.files = fs::read_dir(folder.path()).ok(),
                Some(Err(_)) => (),
                None => {
                    self.directories = None;
                    self.todo = self.found.iter().cloned().collect();
                    self.values
                        .retain(|p, _| self.found.iter().any(|(_, f)| f == p));
                }
            }
        }
        let own_since = self.own_since;
        let count = self.todo.len();
        for _ in 0..count {
            if end.is_some_and(|end| Instant::now() >= end) || stop() {
                break;
            }
            let (modified, path) = self.todo.pop_front().unwrap();
            if !self.values.contains_key(&path) {
                let saved = &self.kept[path.to_string_lossy().as_ref()];
                let mut value = Durations::default();
                if array(saved).len() == 2 && saved[0].as_u64() == Some(modified) {
                    value.seconds = array(&saved[1]).iter().filter_map(Value::as_f64).collect();
                    value.tail = Tail::at_end(&path).unwrap_or_default();
                }
                if own_since.is_some() {
                    if let Some(saved) = self.own_kept.files.get(&path).filter(|s| {
                        s.modified == modified
                            && fs::metadata(&path).is_ok_and(|m| m.len() == s.bytes)
                    }) {
                        value.own = saved.times.clone();
                        value.tail = Tail::at_end(&path).unwrap_or_default();
                    }
                }
                self.values.insert(path.clone(), value);
            }
            let value = self.values.get_mut(&path).unwrap();
            value.modified = modified;

            let mut consume = |row: Option<Boundary>| {
                let Some(row) = row else {
                    value.seconds.clear();
                    value.own.clear();
                    return;
                };
                if row.subtype.as_deref() != Some("compact_boundary") {
                    return;
                }
                let Some(meta) = row.metadata else {
                    return;
                };
                if own_since.is_some() {
                    let when = row.timestamp.as_deref().and_then(super::paths::epoch);
                    if meta.trigger.as_deref() != Some("manual")
                        || when.is_none_or(|t| t == 0.)
                        || meta
                            .pre_tokens
                            .is_none_or(|n| !(181_000. ..=320_000.).contains(&n))
                    {
                        return;
                    }
                }
                if let Some(ms) = meta.duration_ms.filter(|n| *n > 0.) {
                    if own_since.is_some() {
                        let when = row
                            .timestamp
                            .as_deref()
                            .and_then(super::paths::epoch)
                            .unwrap();
                        value.own.push((when, ms / 1000.));
                    } else {
                        value.seconds.push(ms / 1000.);
                    }
                }
            };
            if own_since.is_some() {
                let _ = value.tail.poll_matching(
                    &path,
                    b"\"compact_boundary\"",
                    true,
                    || end.is_some_and(|end| Instant::now() >= end) || stop(),
                    &mut consume,
                );
            } else {
                let _ = value.tail.poll(
                    &path,
                    bytes.min(CHUNK),
                    || end.is_some_and(|end| Instant::now() >= end) || stop(),
                    |line| {
                        let Some(line) = line else {
                            consume(None);
                            return;
                        };
                        if memchr::memmem::find(line, b"\"compact_boundary\"").is_some() {
                            if let Ok(row) = sonic_rs::from_slice(line) {
                                consume(Some(row));
                            }
                        }
                    },
                );
            }
            if fs::metadata(&path).is_ok_and(|m| value.tail.offset < m.len()) {
                self.todo.push_back((modified, path));
            }
        }
        self.complete = self.directories.is_none() && self.todo.is_empty();
        if self.complete && persist && self.own_since.is_none() {
            let current: BTreeMap<_, _> = self
                .values
                .iter()
                .map(|(p, v)| {
                    (
                        p.to_string_lossy().to_string(),
                        json!([v.modified, v.seconds]),
                    )
                })
                .collect();
            let current = json!(current);
            if current != self.kept {
                let _ = super::private::write_atomic(
                    &paths.keep.join("compaction-times.json"),
                    &super::private::dumps(&current),
                    &paths.keep,
                );
                self.kept = current;
            }
        }
        self.usual()
    }
    pub fn own_compactions(paths: &Paths, now: f64, excluded: Vec<f64>) -> Vec<f64> {
        let mut scan = Self {
            own_since: Some(now - 30. * 86400.),
            excluded,
            ..Self::default()
        };
        while !scan.complete {
            scan.advance(paths, now, false, CHUNK, None, || false);
        }

        let current = OwnCache {
            version: 1,
            files: scan
                .values
                .iter()
                .map(|(path, value)| {
                    (
                        path.clone(),
                        OwnFile {
                            modified: value.modified,
                            bytes: fs::metadata(path).map(|m| m.len()).unwrap_or(0),
                            times: value.own.clone(),
                        },
                    )
                })
                .collect(),
        };
        if current != scan.own_kept {
            if let Ok(text) = serde_json::to_string(&current) {
                let _ = super::private::write_atomic(
                    &paths.keep.join("own-compaction-times.json"),
                    &text,
                    &paths.keep,
                );
            }
        }
        let since = now - 30. * 86400.;
        let mut seconds: Vec<_> = scan
            .values
            .values()
            .flat_map(|v| v.own.iter().copied())
            .filter(|(when, _)| {
                *when >= since && !scan.excluded.iter().any(|t| (when - t).abs() <= 300.)
            })
            .map(|(_, seconds)| seconds)
            .collect();
        seconds.sort_by(f64::total_cmp);
        seconds
    }
    fn usual(&self) -> Value {
        let mut seconds: Vec<_> = self
            .values
            .values()
            .flat_map(|v| v.seconds.iter().copied())
            .collect();
        seconds.sort_by(f64::total_cmp);
        if seconds.len() < 3 {
            return Value::Null;
        }
        let pct = |share: f64| {
            let at = (seconds.len() - 1) as f64 * share;
            let i = at as usize;
            seconds[i] + (seconds[(i + 1).min(seconds.len() - 1)] - seconds[i]) * (at - i as f64)
        };
        json!({"low":pct(0.1),"median":pct(0.5),"high":pct(0.9),"n":seconds.len()})
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compaction_scan_yields_resumes_and_obeys_stop() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let file = paths.projects.join("p/s.jsonl");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let row = "{\"subtype\":\"compact_boundary\",\"compactMetadata\":{\"durationMs\":2000}}\n";
        fs::write(&file, row.repeat(10)).unwrap();
        let mut scan = CompactionScan::default();
        scan.step(&paths, 1000., false, 64, Duration::from_secs(1), || true);
        assert!(scan.values.is_empty());
        scan.step(&paths, 1000., false, 64, Duration::from_secs(1), || false);
        assert!(!scan.complete);
        assert_eq!(scan.values[&file].tail.offset, 64);
        for _ in 0..20 {
            scan.step(&paths, 1000., false, 64, Duration::from_secs(1), || false);
        }
        assert!(scan.complete);
        assert_eq!(scan.usual()["n"], 10);
        assert_eq!(scan.usual()["median"], 2.);
    }
}
