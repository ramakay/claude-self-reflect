//! Shift Map likes ingestion (`likes.jsonl` -> `meta["story_kind_prefs:<project>"]`
//! and `meta["shift_ledger:<project>"]`).
//!
//! Record-only in this release: nothing in `csr-engine` reads `story_kind_prefs`
//! to steer Haiku prompts or alter retrieval ranking.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::store::{cap_and_serialize_ledger, ledger_meta_key, load_ledger};
use super::story_map_disabled;
use crate::storage::Storage;

const MAX_READ_BYTES: u64 = 1024 * 1024; // 1 MiB
const PREFS_VERSION: u32 = 1;
const PREFS_MAX_BYTES: usize = 512;
const MAX_EXEMPLAR_TITLE_CHARS: usize = 72;

/// Per-kind preference counters and score.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KindCount {
    pub likes: usize,
    pub expands: usize,
    pub sends: usize,
    pub score: f64,
}

impl KindCount {
    fn compute_score(&mut self) {
        let raw =
            (self.likes as f64) * 1.0 + (self.expands as f64) * 0.25 + (self.sends as f64) * 0.75;
        let clamped = raw.min(5.0);
        self.score = (clamped * 100.0).round() / 100.0;
    }
}

/// All four shift kinds in canonical order (`shift`, `risk`, `decision`, `regression`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KindsPrefs {
    pub shift: KindCount,
    pub risk: KindCount,
    pub decision: KindCount,
    pub regression: KindCount,
}

impl KindsPrefs {
    fn get_mut(&mut self, kind: &str) -> Option<&mut KindCount> {
        match kind {
            "shift" => Some(&mut self.shift),
            "risk" => Some(&mut self.risk),
            "decision" => Some(&mut self.decision),
            "regression" => Some(&mut self.regression),
            _ => None,
        }
    }
}

/// Most recently liked shift entry in the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exemplar {
    pub sig: String,
    pub kind: String,
    pub title: String,
}

/// Pure result of folding a `likes.jsonl` tail against a project's ledger `(sig, kind)` pairs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LikesFold {
    pub liked: HashMap<String, bool>,
    pub kinds: KindsPrefs,
    pub exemplar: Option<Exemplar>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoryKindPrefs {
    v: u32,
    updated_at: i64,
    kinds: KindsPrefs,
    exemplar: Option<Exemplar>,
}

#[derive(Deserialize)]
struct RawLikeLine {
    key: Option<String>,
    source: Option<String>,
    signal: Option<String>,
    title: Option<String>,
    liked: Option<bool>,
}

/// Path to the companion pane's likes log: `$HOME/.claude-self-reflect/csr-companion/likes.jsonl`.
pub fn likes_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| {
        h.join(".claude-self-reflect")
            .join("csr-companion")
            .join("likes.jsonl")
    })
}

fn prefs_meta_key(project: &str) -> String {
    format!("story_kind_prefs:{project}")
}

fn truncate_title(raw: &str) -> String {
    let s: String = raw
        .chars()
        .filter(|&c| !c.is_control())
        .take(MAX_EXEMPLAR_TITLE_CHARS)
        .collect();
    s.trim().to_string()
}

/// Folds `likes.jsonl` text against a project's `(sig, kind)` ledger pairs.
///
/// - Only lines with `source == "story_map"` and `key` starting with `"shift:"` whose
///   `sig` is in `ledger` are counted.
/// - The kind comes from `ledger` (never trusted from the JSONL line).
/// - Explicit likes (`signal == "explicit_like"` or missing `signal`) take the `liked`
///   boolean of the last such line per sig.
/// - Implicit signals (`"expand"`, `"send"`) count at most once per `(sig, signal)`.
/// - `exemplar` is the most recently liked sig that remains `liked == true` at the end.
pub fn fold_likes(text: &str, ledger: &[(String, String)]) -> LikesFold {
    let mut sig_to_kind: HashMap<&str, &str> = HashMap::with_capacity(ledger.len());
    for (sig, kind) in ledger {
        sig_to_kind.insert(sig.as_str(), kind.as_str());
    }

    let mut liked_state: HashMap<String, bool> = HashMap::new();
    let mut explicit_order: Vec<(String, String)> = Vec::new(); // (sig, title)
    let mut seen_expands: HashSet<String> = HashSet::new();
    let mut seen_sends: HashSet<String> = HashSet::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<RawLikeLine>(line) else {
            continue;
        };
        if parsed.source.as_deref() != Some("story_map") {
            continue;
        }
        let Some(key) = parsed.key.as_deref() else {
            continue;
        };
        let Some(sig) = key.strip_prefix("shift:") else {
            continue;
        };
        let Some(&kind) = sig_to_kind.get(sig) else {
            continue;
        };

        let signal = parsed.signal.as_deref().unwrap_or("explicit_like");
        match signal {
            "explicit_like" => {
                let is_liked = parsed.liked.unwrap_or(true);
                liked_state.insert(sig.to_string(), is_liked);
                if is_liked {
                    let title = parsed
                        .title
                        .as_deref()
                        .map(truncate_title)
                        .unwrap_or_default();
                    explicit_order.retain(|(s, _)| s != sig);
                    explicit_order.push((sig.to_string(), title));
                } else {
                    explicit_order.retain(|(s, _)| s != sig);
                }
            }
            "expand" => {
                seen_expands.insert(sig.to_string());
            }
            "send" => {
                seen_sends.insert(sig.to_string());
            }
            _ => continue,
        }
        let _ = kind;
    }

    let mut kinds = KindsPrefs::default();
    for (sig, &is_liked) in &liked_state {
        if is_liked {
            if let Some(&kind) = sig_to_kind.get(sig.as_str()) {
                if let Some(kc) = kinds.get_mut(kind) {
                    kc.likes += 1;
                }
            }
        }
    }
    for sig in &seen_expands {
        if let Some(&kind) = sig_to_kind.get(sig.as_str()) {
            if let Some(kc) = kinds.get_mut(kind) {
                kc.expands += 1;
            }
        }
    }
    for sig in &seen_sends {
        if let Some(&kind) = sig_to_kind.get(sig.as_str()) {
            if let Some(kc) = kinds.get_mut(kind) {
                kc.sends += 1;
            }
        }
    }

    kinds.shift.compute_score();
    kinds.risk.compute_score();
    kinds.decision.compute_score();
    kinds.regression.compute_score();

    let exemplar = explicit_order
        .into_iter()
        .rev()
        .find(|(sig, _)| liked_state.get(sig).copied().unwrap_or(false))
        .and_then(|(sig, title)| {
            let kind = (*sig_to_kind.get(sig.as_str())?).to_string();
            Some(Exemplar { sig, kind, title })
        });

    LikesFold {
        liked: liked_state,
        kinds,
        exemplar,
    }
}

fn read_last_mebibyte(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(String::new());
    }
    let skip_first_line = len > MAX_READ_BYTES;
    let start = len.saturating_sub(MAX_READ_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    if skip_first_line {
        match text.find('\n') {
            Some(idx) => Ok(text[idx + 1..].to_string()),
            None => Ok(String::new()),
        }
    } else {
        Ok(text)
    }
}

fn serialize_prefs_capped(updated_at: i64, fold: &LikesFold) -> Option<String> {
    let mut prefs = StoryKindPrefs {
        v: PREFS_VERSION,
        updated_at,
        kinds: fold.kinds.clone(),
        exemplar: fold.exemplar.clone(),
    };
    let json = serde_json::to_string(&prefs).ok()?;
    if json.len() <= PREFS_MAX_BYTES {
        return Some(json);
    }
    if let Some(ref mut ex) = prefs.exemplar {
        ex.title.clear();
        let json_no_title = serde_json::to_string(&prefs).ok()?;
        if json_no_title.len() <= PREFS_MAX_BYTES {
            return Some(json_no_title);
        }
    }
    prefs.exemplar = None;
    serde_json::to_string(&prefs).ok()
}

/// Syncs `likes.jsonl` at `path` into `meta["story_kind_prefs:<project>"]` and
/// updates `liked` flags on `meta["shift_ledger:<project>"]`.
///
/// Fails open: returns `()` and makes no changes when `story_map_disabled()`,
/// when `path` is missing/unreadable, or when the project's ledger is empty.
pub fn sync_likes(storage: &Storage, project: &str, path: &Path) {
    if story_map_disabled() || project.is_empty() {
        return;
    }
    let mut ledger = load_ledger(storage, project);
    if ledger.entries.is_empty() {
        return;
    }
    let Ok(text) = read_last_mebibyte(path) else {
        return;
    };

    let pairs: Vec<(String, String)> = ledger
        .entries
        .iter()
        .map(|e| (e.sig.clone(), e.kind.clone()))
        .collect();
    let fold = fold_likes(&text, &pairs);

    let now = chrono::Utc::now().timestamp();
    if let Some(prefs_json) = serialize_prefs_capped(now, &fold) {
        let _ = storage.set_meta(&prefs_meta_key(project), &prefs_json);
    }

    let mut ledger_changed = false;
    for entry in &mut ledger.entries {
        let new_liked = fold.liked.get(&entry.sig).copied().unwrap_or(false);
        if entry.liked != new_liked {
            entry.liked = new_liked;
            ledger_changed = true;
        }
    }
    if ledger_changed {
        if let Ok(json) = cap_and_serialize_ledger(ledger) {
            let _ = storage.set_meta(&ledger_meta_key(project), &json);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::{json, Value};

    use super::*;
    use crate::story_map::store::{commit_ledger, load_ledger, LedgerEntry, LedgerPlan};

    fn seed_ledger(storage: &Storage, project: &str, entries: &[(&str, &str)]) {
        let existing: Vec<LedgerEntry> = entries
            .iter()
            .enumerate()
            .map(|(i, (sig, kind))| LedgerEntry {
                sig: (*sig).to_string(),
                kind: (*kind).to_string(),
                claim: format!("Claim {i}"),
                before: "-".into(),
                after: format!("a.rs:f{i}+"),
                receipts: vec!["evo:1".into()],
                first_conv: "c1".into(),
                last_conv: "c1".into(),
                rev: 1,
                first_at: 1000 + i as i64,
                last_at: 1000 + i as i64,
                liked: false,
                reflection_id: Some(format!("refl-{i}")),
            })
            .collect();
        let plan = LedgerPlan {
            existing,
            new_rows: Vec::new(),
        };
        commit_ledger(storage, project, &plan, &[]).unwrap();
    }

    #[test]
    fn last_line_per_key_wins_for_explicit_likes() {
        let ledger = vec![
            ("1111111111111111".to_string(), "risk".to_string()),
            ("2222222222222222".to_string(), "shift".to_string()),
        ];
        let text = concat!(
            r#"{"key":"shift:1111111111111111","source":"story_map","signal":"explicit_like","title":"First title","liked":true}"#,
            "\n",
            r#"{"key":"shift:2222222222222222","source":"story_map","signal":"explicit_like","title":"Second title","liked":true}"#,
            "\n",
            r#"{"key":"shift:1111111111111111","source":"story_map","signal":"explicit_like","title":"Updated first","liked":true}"#,
            "\n"
        );
        let fold = fold_likes(text, &ledger);
        assert_eq!(fold.liked.get("1111111111111111"), Some(&true));
        assert_eq!(fold.liked.get("2222222222222222"), Some(&true));
        assert_eq!(fold.kinds.risk.likes, 1);
        assert_eq!(fold.kinds.shift.likes, 1);
        assert_eq!(
            fold.exemplar,
            Some(Exemplar {
                sig: "1111111111111111".into(),
                kind: "risk".into(),
                title: "Updated first".into(),
            })
        );
    }

    #[test]
    fn unlike_removes_the_like() {
        let ledger = vec![
            ("1111111111111111".to_string(), "risk".to_string()),
            ("2222222222222222".to_string(), "shift".to_string()),
        ];
        let text = concat!(
            r#"{"key":"shift:1111111111111111","source":"story_map","signal":"explicit_like","title":"First","liked":true}"#,
            "\n",
            r#"{"key":"shift:2222222222222222","source":"story_map","signal":"explicit_like","title":"Second","liked":true}"#,
            "\n",
            r#"{"key":"shift:2222222222222222","source":"story_map","signal":"explicit_like","title":"Second","liked":false}"#,
            "\n"
        );
        let fold = fold_likes(text, &ledger);
        assert_eq!(fold.liked.get("1111111111111111"), Some(&true));
        assert_eq!(fold.liked.get("2222222222222222"), Some(&false));
        assert_eq!(fold.kinds.risk.likes, 1);
        assert_eq!(fold.kinds.shift.likes, 0);
        assert_eq!(fold.kinds.shift.score, 0.0);
        assert_eq!(
            fold.exemplar,
            Some(Exemplar {
                sig: "1111111111111111".into(),
                kind: "risk".into(),
                title: "First".into(),
            })
        );
    }

    #[test]
    fn expand_and_send_count_once_per_key() {
        let ledger = vec![("8a3f91c02b11e409".to_string(), "decision".to_string())];
        let text = concat!(
            r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","signal":"expand","liked":true}"#,
            "\n",
            r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","signal":"expand","liked":true}"#,
            "\n",
            r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","signal":"send","liked":true}"#,
            "\n",
            r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","signal":"send","liked":true}"#,
            "\n"
        );
        let fold = fold_likes(text, &ledger);
        assert_eq!(fold.kinds.decision.likes, 0);
        assert_eq!(fold.kinds.decision.expands, 1);
        assert_eq!(fold.kinds.decision.sends, 1);
        assert_eq!(fold.kinds.decision.score, 1.0);
        assert!(fold.exemplar.is_none());
    }

    #[test]
    fn other_sources_and_bad_lines_are_ignored() {
        let ledger = vec![("8a3f91c02b11e409".to_string(), "risk".to_string())];
        let text = concat!(
            "not json at all\n",
            r#"{"key":"shift:8a3f91c02b11e409","source":"changed","liked":true}"#,
            "\n",
            r#"{"key":"dream:8a3f91c02b11e409","source":"story_map","liked":true}"#,
            "\n",
            r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","signal":"unknown_sig","liked":true}"#,
            "\n",
            r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","kind":"regression","signal":"explicit_like","title":"Kept","liked":true}"#,
            "\n"
        );
        let fold = fold_likes(text, &ledger);
        // Kind comes from ledger ("risk"), not the JSONL line ("regression").
        assert_eq!(fold.kinds.risk.likes, 1);
        assert_eq!(fold.kinds.regression.likes, 0);
        assert_eq!(fold.exemplar.as_ref().unwrap().kind, "risk");
    }

    #[test]
    fn likes_for_another_projects_sigs_are_ignored() {
        let ledger = vec![("aaaaaaaaaaaaaaaa".to_string(), "shift".to_string())];
        let text = concat!(
            r#"{"key":"shift:bbbbbbbbbbbbbbbb","source":"story_map","signal":"explicit_like","title":"Other proj","liked":true}"#,
            "\n",
            r#"{"key":"shift:bbbbbbbbbbbbbbbb","source":"story_map","signal":"send","liked":true}"#,
            "\n"
        );
        let fold = fold_likes(text, &ledger);
        assert!(fold.liked.is_empty());
        assert_eq!(fold.kinds.shift.likes, 0);
        assert_eq!(fold.kinds.shift.sends, 0);
        assert!(fold.exemplar.is_none());
    }

    #[test]
    fn only_the_last_mebibyte_is_read() {
        let storage = Storage::open_memory().unwrap();
        seed_ledger(
            &storage,
            "csr",
            &[("1111111111111111", "risk"), ("2222222222222222", "shift")],
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("likes.jsonl");
        let mut f = File::create(&path).unwrap();

        // Write an old like at byte 0, followed by > 1 MiB of padding lines, then a recent like.
        writeln!(
            f,
            r#"{{"key":"shift:1111111111111111","source":"story_map","signal":"explicit_like","title":"Ancient","liked":true}}"#
        )
        .unwrap();
        let filler_line = format!(
            r#"{{"key":"other:pad","source":"other","title":"{}","liked":true}}"#,
            "x".repeat(200)
        );
        let mut written = 0usize;
        while written <= (MAX_READ_BYTES as usize) + 4096 {
            writeln!(f, "{filler_line}").unwrap();
            written += filler_line.len() + 1;
        }
        writeln!(
            f,
            r#"{{"key":"shift:2222222222222222","source":"story_map","signal":"explicit_like","title":"Recent","liked":true}}"#
        )
        .unwrap();
        f.flush().unwrap();

        sync_likes(&storage, "csr", &path);

        let ledger = load_ledger(&storage, "csr");
        let e1 = ledger
            .entries
            .iter()
            .find(|e| e.sig == "1111111111111111")
            .unwrap();
        let e2 = ledger
            .entries
            .iter()
            .find(|e| e.sig == "2222222222222222")
            .unwrap();
        assert!(!e1.liked, "old like before the last 1 MiB must not count");
        assert!(e2.liked, "recent like in the last 1 MiB must count");
    }

    #[test]
    fn prefs_json_matches_the_contract() {
        let storage = Storage::open_memory().unwrap();
        seed_ledger(
            &storage,
            "csr",
            &[("8a3f91c02b11e409", "risk"), ("19c044a8e011b201", "shift")],
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("likes.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","kind":"risk","signal":"explicit_like","weight":1.0,"conv":"c1","rev":4,"title":"pane.py:session_story~ -> meta:story_map+","at":1760400120,"liked":true}"#,
                "\n",
                r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","signal":"expand","liked":true}"#,
                "\n",
                r#"{"key":"shift:19c044a8e011b201","source":"story_map","signal":"send","liked":true}"#,
                "\n"
            ),
        )
        .unwrap();

        sync_likes(&storage, "csr", &path);

        let raw = storage.get_meta("story_kind_prefs:csr").unwrap().unwrap();
        let mut val: Value = serde_json::from_str(&raw).unwrap();
        let updated_at = val["updated_at"].as_i64().expect("updated_at int");
        assert!(updated_at > 0);
        val["updated_at"] = json!(1760400200);

        assert_eq!(
            val,
            json!({
                "v": 1,
                "updated_at": 1760400200,
                "kinds": {
                    "shift": {"likes": 0, "expands": 0, "sends": 1, "score": 0.75},
                    "risk": {"likes": 1, "expands": 1, "sends": 0, "score": 1.25},
                    "decision": {"likes": 0, "expands": 0, "sends": 0, "score": 0.0},
                    "regression": {"likes": 0, "expands": 0, "sends": 0, "score": 0.0}
                },
                "exemplar": {
                    "sig": "8a3f91c02b11e409",
                    "kind": "risk",
                    "title": "pane.py:session_story~ -> meta:story_map+"
                }
            })
        );
    }

    #[test]
    fn prefs_stay_under_512_bytes() {
        let mut entries = Vec::new();
        for i in 0..12usize {
            entries.push((format!("{i:016x}"), "regression".to_string()));
        }
        let mut lines = String::new();
        for (sig, _) in &entries {
            lines.push_str(&format!(
                r#"{{"key":"shift:{sig}","source":"story_map","signal":"explicit_like","title":"{}","liked":true}}"#,
                "T".repeat(120)
            ));
            lines.push('\n');
        }
        let fold = fold_likes(&lines, &entries);
        // Score is clamped at 5.0 even with 12 likes.
        assert_eq!(fold.kinds.regression.likes, 12);
        assert_eq!(fold.kinds.regression.score, 5.0);
        assert_eq!(fold.exemplar.as_ref().unwrap().title.len(), 72);

        let json = serialize_prefs_capped(1760400200, &fold).unwrap();
        assert!(json.len() <= 512, "len was {}", json.len());
    }

    #[test]
    fn ledger_liked_flag_follows_the_file() {
        let storage = Storage::open_memory().unwrap();
        seed_ledger(&storage, "csr", &[("8a3f91c02b11e409", "risk")]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("likes.jsonl");

        std::fs::write(
            &path,
            r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","signal":"explicit_like","title":"T","liked":true}"#,
        )
        .unwrap();
        sync_likes(&storage, "csr", &path);
        assert!(load_ledger(&storage, "csr").entries[0].liked);

        std::fs::write(
            &path,
            concat!(
                r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","signal":"explicit_like","title":"T","liked":true}"#,
                "\n",
                r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","signal":"explicit_like","title":"T","liked":false}"#,
                "\n"
            ),
        )
        .unwrap();
        sync_likes(&storage, "csr", &path);
        assert!(!load_ledger(&storage, "csr").entries[0].liked);
    }

    #[test]
    fn missing_file_changes_nothing() {
        let storage = Storage::open_memory().unwrap();
        seed_ledger(&storage, "csr", &[("8a3f91c02b11e409", "risk")]);
        let before_raw = storage.get_meta("shift_ledger:csr").unwrap().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does_not_exist.jsonl");
        sync_likes(&storage, "csr", &missing);

        assert!(storage.get_meta("story_kind_prefs:csr").unwrap().is_none());
        assert_eq!(
            storage.get_meta("shift_ledger:csr").unwrap().unwrap(),
            before_raw
        );
    }

    #[test]
    fn kill_switch_changes_nothing() {
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let storage = Storage::open_memory().unwrap();
        seed_ledger(&storage, "csr", &[("8a3f91c02b11e409", "risk")]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("likes.jsonl");
        std::fs::write(
            &path,
            r#"{"key":"shift:8a3f91c02b11e409","source":"story_map","signal":"explicit_like","title":"T","liked":true}"#,
        )
        .unwrap();

        std::env::set_var("CSR_NO_STORY_MAP", "1");
        sync_likes(&storage, "csr", &path);
        std::env::remove_var("CSR_NO_STORY_MAP");

        assert!(storage.get_meta("story_kind_prefs:csr").unwrap().is_none());
        assert!(!load_ledger(&storage, "csr").entries[0].liked);
    }
}
