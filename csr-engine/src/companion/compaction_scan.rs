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

#[derive(Default)]
struct Durations {
    tail: Tail,
    seconds: Vec<f64>,
    modified: u64,
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
        if self.complete && now - self.at < 300. {
            return self.usual();
        }
        let end = Instant::now() + duration;
        if !self.started || self.complete {
            self.at = now;
            self.complete = false;
            self.started = true;
            self.found.clear();
            self.directories = fs::read_dir(&paths.projects).ok();
            self.kept = read_json(&paths.keep.join("compaction-times.json"));
        }
        while self.directories.is_some() && Instant::now() < end && !stop() {
            if let Some(files) = &mut self.files {
                match files.next() {
                    Some(Ok(file)) => {
                        let path = file.path();
                        if path.extension().is_some_and(|s| s == "jsonl") {
                            // fs::metadata follows a symlinked transcript to the file it names.
                            let time = fs::metadata(&path).and_then(|m| m.modified());
                            if let Ok(time) = time {
                                let time = time.duration_since(UNIX_EPOCH).unwrap_or_default();
                                self.found.push((time.as_nanos() as u64, path));
                                self.found.sort_by(|a, b| b.cmp(a));
                                self.found.truncate(60);
                            }
                        }
                        continue;
                    }
                    Some(Err(_)) => continue, // one unreadable entry, not the whole folder
                    None => self.files = None,
                }
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
        let count = self.todo.len();
        for _ in 0..count {
            if Instant::now() >= end || stop() {
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
                self.values.insert(path.clone(), value);
            }
            let value = self.values.get_mut(&path).unwrap();
            value.modified = modified;
            let _ = value.tail.poll(
                &path,
                bytes.min(CHUNK),
                || Instant::now() >= end || stop(),
                |line| {
                    let Some(line) = line else {
                        value.seconds.clear();
                        return;
                    };
                    if !line.windows(18).any(|w| w == b"\"compact_boundary\"") {
                        return;
                    }
                    if let Ok(row) = sonic_rs::from_slice::<Value>(line) {
                        if row["subtype"] == "compact_boundary" {
                            if let Some(ms) = row["compactMetadata"]["durationMs"]
                                .as_f64()
                                .filter(|v| *v > 0.)
                            {
                                value.seconds.push(ms / 1000.);
                            }
                        }
                    }
                },
            );
            if fs::metadata(&path).is_ok_and(|m| value.tail.offset < m.len()) {
                self.todo.push_back((modified, path));
            }
        }
        self.complete = self.directories.is_none() && self.todo.is_empty();
        if self.complete && persist {
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
    #[test]
    fn a_symlinked_transcript_is_dated_by_the_file_it_names() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().join("home"));
        let real = d.path().join("elsewhere.jsonl");
        fs::write(&real, "{}\n").unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        fs::File::options()
            .write(true)
            .open(&real)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let link = paths.projects.join("p/s.jsonl");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut scan = CompactionScan::default();
        scan.step(&paths, 1000., false, 64, Duration::from_secs(1), || false);
        let seconds = Duration::from_nanos(scan.found[0].0).as_secs();
        assert_eq!((seconds, &scan.found[0].1), (1_000, &link));
    }
}
