//! PreCompact hook — snapshots the compact anchor before compaction.
//!
//! 1. For a main session, reads the transcript with the importer's parser (no
//!    embedding, no chunk writes, so no wait on the daemon) and files the compact
//!    snapshot the post-compact recap reads. A subagent's PreCompact files nothing.
//! 2. Triggers one dream cycle (all sessions).
//!
//! The hook does not index the transcript: the file survives compaction, and the
//! daemon's watcher and the Stop worker at the end of every turn import it.
//!
//! The session's story is written after the compaction instead, by SessionStart, from the
//! summary Claude Code writes as it compacts: that needs no model call.
//!
//! Exits 0 on every path. It prints one thing, and only when the veto below holds:
//! `{"decision":"block","reason":...}`, which stops an idle auto-compaction that
//! would collide with the companion's own. Every other path prints nothing.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;

use super::turn_marks::TurnMarks;
use super::HookInput;
use crate::companion::app::state::{BAND_HIGH, BAND_LOW};
use crate::companion::paths::Paths;
use crate::engine::Engine;

/// A turn is not in flight once the last Stop is this much older than now.
const IDLE_AFTER: Duration = Duration::from_secs(30);
/// The companion mod's marker must be younger than this to count as armed.
const MARKER_LIFE: Duration = Duration::from_secs(5 * 60);
/// One veto per session per this long.
const VETO_COOLDOWN: Duration = Duration::from_secs(10 * 60);

/// The PreCompact veto (00-design.md, "Rule for the rewrite", S10 PASS).
///
/// True only when ALL hold:
/// - `trigger == "auto"`;
/// - no turn is in flight: the last Stop is later than the last
///   UserPromptSubmit, and that Stop is older than 30 s;
/// - the companion mod is loaded and armed (`mod_armed`);
/// - context is inside the band (`in_band`);
/// - no veto fired for this session in the last 10 min.
///
/// A missing mark, a clock that runs backwards or any doubt is `false`.
pub fn decide_veto(
    input: &HookInput,
    marks: &TurnMarks,
    mod_armed: bool,
    in_band: bool,
    now: SystemTime,
) -> bool {
    if input.trigger.as_deref() != Some("auto") {
        return false;
    }
    let (Some(last_prompt), Some(last_stop)) = (marks.last_prompt, marks.last_stop) else {
        return false;
    };
    if last_stop <= last_prompt {
        return false;
    }
    match now.duration_since(last_stop) {
        Ok(idle) if idle > IDLE_AFTER => {}
        _ => return false,
    }
    if !mod_armed || !in_band {
        return false;
    }
    match marks.last_veto {
        None => true,
        Some(veto) => now
            .duration_since(veto)
            .is_ok_and(|since| since > VETO_COOLDOWN),
    }
}

/// Seconds since the epoch of a marker's `at` (seconds or milliseconds, a
/// number or a numeric string, as `companion::sources::compacting_info` reads
/// it). `None` when it is absent or not a time.
fn marker_at(marker: &serde_json::Value) -> Option<f64> {
    let at = marker["at"]
        .as_f64()
        .or_else(|| marker["at"].as_str().and_then(|text| text.parse().ok()))?;
    let at = if at > 1e11 { at / 1000. } else { at };
    (at.is_finite() && at > 0.).then_some(at)
}

/// Whether the companion mod is loaded and armed for this session: its
/// `compacting/<sid>.json` marker, or its `compact-plan/<sid>.json` plan (any
/// state but `none`, which is the mod taking the plan down), is younger than
/// 5 min. Read-only; the companion owns both files.
pub fn mod_armed(paths: &Paths, session_id: &str, now: SystemTime) -> bool {
    let now_secs = now
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default();
    let young = |folder: &str, plan: bool| -> bool {
        let Some(file) = paths.kept_file(folder, session_id) else {
            return false;
        };
        let marker = crate::companion::paths::read_json(&file);
        if !marker.is_object() || (plan && marker["state"].as_str() == Some("none")) {
            return false;
        }
        marker_at(&marker).is_some_and(|at| {
            let age = now_secs - at;
            (0. ..=MARKER_LIFE.as_secs_f64()).contains(&age)
        })
    };
    young("compacting", false) || young("compact-plan", true)
}

/// Whether the session's last measured context is inside the companion's band.
/// Reads the transcript the way the companion does and writes nothing.
fn context_in_band(paths: &Paths, session_id: &str) -> bool {
    let mut session = crate::companion::session::Session::new(session_id);
    session.read_transcript(paths);
    session.data["main"]
        .as_array()
        .and_then(|turns| turns.last())
        .and_then(|turn| turn[1].as_f64())
        .is_some_and(|context| (BAND_LOW..=BAND_HIGH).contains(&context))
}

/// Run the veto for this input; on `true` print the block and mark it.
/// Any error is `false`, so the hook falls through to its usual exit 0.
fn veto_and_mark(input: &HookInput, engine: &Engine) -> bool {
    use std::io::Write;

    // The rule has no subagent exemption: a subagent's PreCompact carries the
    // parent's session id and is judged on the parent's marks.
    let Some(session_id) = input.session_id.as_deref().filter(|id| !id.is_empty()) else {
        return false;
    };
    if input.trigger.as_deref() != Some("auto") {
        return false;
    }
    let now = SystemTime::now();
    let marks = super::turn_marks::read(engine.data_dir(), session_id);
    let paths = Paths::from_env();
    // Cheap facts first: the transcript is read only when every other
    // condition already holds.
    let armed = mod_armed(&paths, session_id, now);
    let precheck = decide_veto(input, &marks, armed, true, now);
    if !precheck {
        return false;
    }
    let in_band = context_in_band(&paths, session_id);
    if !decide_veto(input, &marks, armed, in_band, now) {
        return false;
    }
    let reason = "CSR is holding this idle auto-compaction: the companion compacts this \
                  session at a seam instead.";
    let body = serde_json::json!({"decision": "block", "reason": reason});
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{body}").and_then(|()| out.flush()).is_err() {
        return false;
    }
    super::turn_marks::mark_veto(engine.data_dir(), session_id);
    true
}

/// Only compact recaps consume this snapshot. The Stop episode remains the
/// source of the first-task anchor for startup, resume and other sessions.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct CompactSnapshot {
    /// The anchor the snapshot's `completions` and `completions_untied` are
    /// relative to (the episode's `anchor_request` at PreCompact). A snapshot
    /// written before this field existed carries `None` and never matches.
    #[serde(default)]
    pub anchor_request: Option<String>,
    pub anchor: Option<super::stop::CompactAnchor>,
    /// The session's last typed ask when the snapshot was taken, `None` when
    /// there was none. It travels with the anchor: an ask saved with an older
    /// task must not be shown after a newer one. A snapshot written before
    /// this field existed reads as no ask.
    #[serde(default)]
    pub latest_ask: Option<String>,
    /// The transcript position this snapshot read (`Episode::watermark`),
    /// recorded the same way the Stop episode records its own. The overlay
    /// applies only when it is clearly ahead of the stored episode's.
    #[serde(default)]
    pub watermark: Option<String>,
    pub completions: Vec<super::stop::CompletionReceipt>,
    pub completions_untied: bool,
    /// The receipts produced under `anchor_request` (`Episode::anchor_receipts`).
    /// A snapshot written before this field existed carries `None`.
    #[serde(default)]
    pub anchor_receipts: Option<Vec<usize>>,
}

/// The conversation rows the compact snapshot is built from: the rows the importer
/// hands its observer, read with the importer's own parser. Nothing is embedded or
/// stored, so this never waits on the daemon; the parsed chunks are dropped.
fn transcript_rows(input: &HookInput, cwd: &Path) -> Vec<serde_json::Value> {
    let Some(canonical) = super::validated_transcript_path(input) else {
        return Vec::new();
    };
    let project = super::hook_project_name(cwd);
    let mut rows = Vec::new();
    let mut observe = |row: &serde_json::Value| {
        // Progress payloads dominate large transcripts and contain no recap
        // evidence. Keep only the row types used by the episode extractor.
        if matches!(
            row.get("type").and_then(|v| v.as_str()),
            Some("user" | "human" | "assistant" | "system")
        ) {
            rows.push(row.clone());
        }
    };
    if let Err(error) = crate::import::parse_jsonl_file_resuming(
        &canonical,
        &project,
        None,
        Some(&mut observe),
        None,
    ) {
        eprintln!("CSR: transcript parse failed (non-fatal): {error}");
    }
    rows
}

/// Handle the precompact hook.
/// Always returns Ok(()): a failure here never blocks compaction (E-1 fix).
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    // A vetoed compaction does not happen, so there is no snapshot to take and
    // no consolidation boundary to dream on. Every other path is unchanged.
    if veto_and_mark(input, engine) {
        return Ok(());
    }
    // A subagent's PreCompact carries the PARENT's session id: the snapshot below would
    // be filed under the parent and feed its next compact recap with the subagent's rows.
    let subagent = input.subagent();
    if let Some(agent_type) = subagent.as_deref() {
        super::log_subagent_skip("precompact", cwd, agent_type);
    }
    if let Some(session_id) = input.session_id.as_deref().filter(|_| subagent.is_none()) {
        let rows = transcript_rows(input, cwd);
        if !rows.is_empty() {
            let (sanitized, _) = crate::import::sanitize_messages_for_search(&rows);
            let episode =
                super::stop::extract_episode_from_messages(&sanitized, &rows, session_id, "");
            let snapshot = CompactSnapshot {
                anchor_request: episode.anchor_request,
                anchor: episode.compact_anchor,
                latest_ask: episode.latest_ask,
                watermark: episode.watermark,
                completions: episode.completions,
                completions_untied: episode.completions_untied,
                anchor_receipts: episode.anchor_receipts,
            };
            let saved = serde_json::to_string(&snapshot)
                .map_err(anyhow::Error::from)
                .and_then(|json| {
                    engine
                        .storage()
                        .set_meta(&format!("compact_anchor:{session_id}"), &json)
                });
            if let Err(error) = saved {
                eprintln!("CSR: compact anchor refresh failed (non-fatal): {error}");
            }
        }
    }

    // v10: a compaction is a natural consolidation boundary. Trigger exactly
    // one dream cycle over the already-indexed corpus, detached and
    // fire-and-forget, so it never blocks compaction. It advertises the live
    // "dreaming" statusline marker for its duration and self-skips if a dream
    // is already running (marker guard in `dream::run_dream_marked`).
    // Independent of the idle-gated daemon cadence — a compaction is an
    // explicit trigger. Opt out with CSR_NO_DREAM_ON_COMPACT / CSR_NO_DREAMING.
    crate::dream::spawn_detached_compact_dream();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{embeddings::EmbeddingEngine, search::SearchEngine, storage::Storage};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    const FIRST: &str = "Design the discussion side pane for users.";
    const LATEST: &str = "Fix the database durability regression before release.";

    async fn exercise(padding_rows: usize, unchanged: bool) -> (String, f64, u64) {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = Engine::from_parts(
            storage.clone(),
            Arc::new(EmbeddingEngine::disabled()),
            Arc::new(RwLock::new(SearchEngine::new(16))),
            temp.path().to_path_buf(),
        );
        let path = temp.path().join("r8-compact.jsonl");
        // Row times are what orders the Stop episode against the snapshot.
        let first = serde_json::json!({"type":"user","timestamp":"2026-10-21T10:00:00Z","message":{"content":FIRST}}).to_string();
        let done = serde_json::json!({"type":"assistant","timestamp":"2026-10-21T10:01:00Z","message":{"content":"Done.","stop_reason":"end_turn"}}).to_string();
        // A recap that names only the task abstains, and the fragment block does not
        // follow the snapshot. The session landed one commit (in the transcript, so
        // the snapshot sees it too): the recap keeps its head clause, the task this
        // test reads, and adds `Settled:`.
        let call = serde_json::json!({"type":"assistant","timestamp":"2026-10-21T10:00:20Z","message":{"content":[{"type":"tool_use","id":"toolu_c1","name":"Bash","input":{"command":"git commit -m x"}}]}}).to_string();
        let result = serde_json::json!({"type":"user","timestamp":"2026-10-21T10:00:30Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_c1","content":"[main abc1234] fix(auth): keep form errors"}]}}).to_string();
        let ep = crate::hooks::stop::extract_episode(
            &[&first, &call, &result, &done],
            "r8-compact",
            "fixture",
        );
        assert!(ep.watermark.is_some());
        assert_eq!(ep.completions.len(), 1);
        storage
            .insert_reflection(
                "r8-episode",
                &serde_json::to_string(&ep).unwrap(),
                &crate::hooks::stop::episode_tags(&ep),
                &[0.0; 384],
            )
            .unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "{first}\n{call}\n{result}\n{done}").unwrap();
        let padding = serde_json::json!({"type":"progress","data":"x".repeat(65_000)}).to_string();
        for _ in 0..padding_rows {
            writeln!(file, "{padding}").unwrap();
        }
        writeln!(
            file,
            "{}",
            serde_json::json!({"type":"user","timestamp":"2026-10-21T10:30:00Z","message":{"content":LATEST}})
        )
        .unwrap();
        drop(file);
        let canonical = path.canonicalize().unwrap();
        // Existing imported chunks keep this benchmark independent of embeddings.
        storage
            .upsert_import_state_explicit(canonical.to_str().unwrap(), "r8-compact", 100, "fixture")
            .unwrap();
        if unchanged {
            storage.mark_file_imported(&canonical, 100).unwrap();
        }
        let input = HookInput {
            session_id: Some("r8-compact".into()),
            transcript_path: Some(canonical.to_string_lossy().into()),
            ..Default::default()
        };
        let start = std::time::Instant::now();
        handle(&input, &engine, temp.path()).await.unwrap();
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        let block = crate::hooks::session_start::format_session_start_tier0_for_session(
            &storage,
            "fixture",
            &ep,
            &[],
            "now",
            Some("r8-compact"),
            Some("compact"),
        );
        // Other startup paths must keep their first-task anchor.
        for source in ["startup", "resume", "clear"] {
            let block = crate::hooks::session_start::format_session_start_tier0_for_session(
                &storage,
                "fixture",
                &ep,
                &[],
                "now",
                Some("r8-compact"),
                Some(source),
            );
            assert!(block.contains(FIRST), "{block}");
        }
        (block, elapsed, std::fs::metadata(path).unwrap().len())
    }

    /// Run `handle` on a two-row transcript with the given hook input extras and
    /// return the stored compact snapshot for session `s-parent`.
    fn snapshot_after_precompact(agent: Option<(&str, &str)>) -> Option<String> {
        let temp = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = Engine::from_parts(
            storage.clone(),
            Arc::new(EmbeddingEngine::disabled()),
            Arc::new(RwLock::new(SearchEngine::new(16))),
            temp.path().to_path_buf(),
        );
        let path = temp.path().join("s-parent.jsonl");
        let user = serde_json::json!({"type":"user","message":{"content":LATEST}}).to_string();
        let done = serde_json::json!({"type":"assistant","message":{"content":"Done.","stop_reason":"end_turn"}}).to_string();
        std::fs::write(&path, format!("{user}\n{done}\n")).unwrap();
        let canonical = path.canonicalize().unwrap();
        let input = HookInput {
            session_id: Some("s-parent".into()),
            transcript_path: Some(canonical.to_string_lossy().into()),
            agent_id: agent.map(|(id, _)| id.to_string()),
            agent_type: agent.map(|(_, kind)| kind.to_string()),
            ..Default::default()
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(handle(&input, &engine, temp.path()))
            .unwrap();
        storage.get_meta("compact_anchor:s-parent").unwrap()
    }

    #[test]
    fn f26_subagent_precompact_files_nothing_under_the_parent_session() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        std::env::set_var("CSR_NO_DREAM_ON_COMPACT", "1");
        let main = snapshot_after_precompact(None);
        let sub = snapshot_after_precompact(Some(("agent-1", "general-purpose")));
        std::env::remove_var("CSR_NO_DREAM_ON_COMPACT");
        assert!(
            main.is_some(),
            "a main-session precompact still files its snapshot"
        );
        assert!(sub.is_none(), "a subagent precompact must not: {sub:?}");
    }

    #[test]
    fn r8_precompact_refreshes_task_before_stop() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        std::env::set_var("CSR_NO_DREAM_ON_COMPACT", "1");
        std::env::remove_var("CSR_NO_RECAP");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for unchanged in [false, true] {
            let (block, _, _) = runtime.block_on(exercise(0, unchanged));
            assert!(block.contains(LATEST), "{block}");
        }
        std::env::remove_var("CSR_NO_DREAM_ON_COMPACT");
    }

    #[test]
    #[ignore = "65 MB PreCompact latency measurement; run explicitly in release mode"]
    fn r8_precompact_timing_65mb() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        std::env::set_var("CSR_NO_DREAM_ON_COMPACT", "1");
        std::env::remove_var("CSR_NO_RECAP");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for _ in 0..5 {
            let (_, elapsed, bytes) = runtime.block_on(exercise(1000, false));
            eprintln!("precompact_bytes={bytes} precompact_ms={elapsed:.3}");
        }
        std::env::remove_var("CSR_NO_DREAM_ON_COMPACT");
    }

    // --- the veto rule: one truth table, one flipped condition at a time ---

    fn input(trigger: Option<&str>) -> HookInput {
        serde_json::from_value(serde_json::json!({
            "session_id": "sess",
            "trigger": trigger,
        }))
        .unwrap()
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    const NOW: u64 = 1_793_000_000;

    /// An idle session: prompt at NOW-600, stop at NOW-300, no veto yet.
    fn idle_marks() -> TurnMarks {
        TurnMarks {
            last_prompt: Some(at(NOW - 600)),
            last_stop: Some(at(NOW - 300)),
            last_veto: None,
        }
    }

    #[test]
    fn veto_holds_for_an_idle_auto_compaction_with_an_armed_mod_in_band() {
        assert!(decide_veto(
            &input(Some("auto")),
            &idle_marks(),
            true,
            true,
            at(NOW)
        ));
        // A veto older than 10 min does not count.
        let old_veto = TurnMarks {
            last_veto: Some(at(NOW - 601)),
            ..idle_marks()
        };
        assert!(decide_veto(
            &input(Some("auto")),
            &old_veto,
            true,
            true,
            at(NOW)
        ));
    }

    #[test]
    fn veto_is_false_when_any_single_condition_is_flipped() {
        let auto = input(Some("auto"));
        let now = at(NOW);
        // Not armed, not in band.
        assert!(!decide_veto(&auto, &idle_marks(), false, true, now));
        assert!(!decide_veto(&auto, &idle_marks(), true, false, now));
        // A turn in flight: the last prompt is later than the last stop.
        let in_flight = TurnMarks {
            last_prompt: Some(at(NOW - 100)),
            last_stop: Some(at(NOW - 300)),
            last_veto: None,
        };
        assert!(!decide_veto(&auto, &in_flight, true, true, now));
        // Equal marks are not "later".
        let tied = TurnMarks {
            last_prompt: Some(at(NOW - 300)),
            last_stop: Some(at(NOW - 300)),
            last_veto: None,
        };
        assert!(!decide_veto(&auto, &tied, true, true, now));
        // The Stop is too fresh: exactly 30 s is not older than 30 s.
        for age in [0, 10, 30] {
            let fresh = TurnMarks {
                last_stop: Some(at(NOW - age)),
                ..idle_marks()
            };
            assert!(!decide_veto(&auto, &fresh, true, true, now), "{age}s");
        }
        let just_idle = TurnMarks {
            last_stop: Some(at(NOW - 31)),
            ..idle_marks()
        };
        assert!(decide_veto(&auto, &just_idle, true, true, now));
        // A veto in the last 10 min, and one stamped in the future.
        for veto in [NOW - 599, NOW - 600, NOW - 1, NOW, NOW + 60] {
            let vetoed = TurnMarks {
                last_veto: Some(at(veto)),
                ..idle_marks()
            };
            assert!(!decide_veto(&auto, &vetoed, true, true, now), "{veto}");
        }
        // A Stop stamped in the future (a clock that ran backwards).
        let future_stop = TurnMarks {
            last_prompt: Some(at(NOW - 10)),
            last_stop: Some(at(NOW + 50)),
            last_veto: None,
        };
        assert!(!decide_veto(&auto, &future_stop, true, true, now));
    }

    #[test]
    fn veto_never_fires_for_manual_precompute_or_a_missing_trigger() {
        for trigger in [
            Some("manual"),
            Some("precompute"),
            Some("AUTO"),
            Some(""),
            None,
        ] {
            assert!(
                !decide_veto(&input(trigger), &idle_marks(), true, true, at(NOW)),
                "{trigger:?}"
            );
        }
    }

    #[test]
    fn veto_is_false_when_a_mark_is_missing() {
        let auto = input(Some("auto"));
        let now = at(NOW);
        for marks in [
            TurnMarks::default(),
            TurnMarks {
                last_prompt: None,
                last_stop: Some(at(NOW - 300)),
                last_veto: None,
            },
            TurnMarks {
                last_prompt: Some(at(NOW - 600)),
                last_stop: None,
                last_veto: None,
            },
        ] {
            assert!(!decide_veto(&auto, &marks, true, true, now), "{marks:?}");
        }
    }

    #[test]
    fn the_mod_is_armed_by_a_young_compacting_marker_or_a_live_plan() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path().to_path_buf());
        let now = at(NOW);
        let write = |folder: &str, body: serde_json::Value| {
            let file = paths.kept_file(folder, "sess").unwrap();
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, body.to_string()).unwrap();
        };
        // No marker, no mod.
        assert!(!mod_armed(&paths, "sess", now));
        // A compacting marker 4 min old arms it; 6 min old does not.
        write("compacting", serde_json::json!({"at": NOW - 240}));
        assert!(mod_armed(&paths, "sess", now));
        write("compacting", serde_json::json!({"at": NOW - 360}));
        assert!(!mod_armed(&paths, "sess", now));
        // Milliseconds and a numeric string read the same way.
        write("compacting", serde_json::json!({"at": (NOW - 60) * 1000}));
        assert!(mod_armed(&paths, "sess", now));
        write(
            "compacting",
            serde_json::json!({"at": (NOW - 60).to_string()}),
        );
        assert!(mod_armed(&paths, "sess", now));
        // A marker with no time is not armed.
        write("compacting", serde_json::json!({"state": "x"}));
        assert!(!mod_armed(&paths, "sess", now));
        // A plan arms it unless the mod took it down.
        write(
            "compact-plan",
            serde_json::json!({"at": NOW - 30, "state": "countdown", "text": "t"}),
        );
        assert!(mod_armed(&paths, "sess", now));
        write(
            "compact-plan",
            serde_json::json!({"at": NOW - 30, "state": "none", "text": "t"}),
        );
        assert!(!mod_armed(&paths, "sess", now));
        write(
            "compact-plan",
            serde_json::json!({"at": NOW - 400, "state": "plan", "text": "t"}),
        );
        assert!(!mod_armed(&paths, "sess", now));
        // Another session's marker, and an id that is not a file name.
        write("compacting", serde_json::json!({"at": NOW - 10}));
        assert!(!mod_armed(&paths, "other", now));
        assert!(!mod_armed(&paths, "../sess", now));
        assert!(!mod_armed(&paths, "", now));
    }
}
