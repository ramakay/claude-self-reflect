//! Setup step 7: the dreaming consent, asked once.
//!
//! The question can come up in three places: `install.sh`'s "Run setup now?" prompt (which
//! discloses dreams and `claude -p`), this step, and the in-session "CSR setup 1/2 Dreams" card.
//! A yes at the installer reaches here as `CSR_DREAMS_CONSENT=yes`, so setup does not ask again,
//! and setup's answer is written where the card looks, so the card does not ask again either.
//!
//! What each reader sees for a "yes" (installer, or `y` here):
//! - the dreamer reads the `dream_consent` meta row (`dream_cadence::consent_declined`): granted;
//! - the card reads the machine row `setup:answer:dreams` in the companion store: present, so the
//!   step is not due;
//! - the pane reads `dreams_on` from `~/.claude/settings.json`, which is on unless the plugin's
//!   `dreams` option was set to false: on.
//!
//! A decline is recorded for the dreamer only. The plugin option's default is "on" and the card is
//! the one place that writes it, so the row is left out: the card still shows for a decline and is
//! the off/on switch for the pane. A silent default (no tty, no `CSR_DREAMS_CONSENT`) is not an
//! answer from a person either, so it records consent for the dreamer, as it always did, and
//! leaves the card to ask.

use std::io::{BufRead, IsTerminal};

use anyhow::Result;
use serde_json::json;

use crate::companion::store::{self, Store, MACHINE};
use crate::daemon::dream_cadence;
use crate::dream::policy::{self, EffortTier, NightEstimate};
use crate::storage::Storage;

/// The card's answer row for the dreams step.
pub(crate) const CARD_ANSWER_KEY: &str = "setup:answer:dreams";

/// What `CSR_DREAMS_CONSENT` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Preset {
    Yes,
    No,
    /// Not set, empty, or not a word this step knows: behave as if it were not set.
    Unset,
}

/// `yes`/`1`/`true` and `no`/`0`/`false`, any case, around which spaces do not matter.
pub(crate) fn parse_preset(value: Option<&str>) -> Preset {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("yes" | "1" | "true") => Preset::Yes,
        Some("no" | "0" | "false") => Preset::No,
        _ => Preset::Unset,
    }
}

/// Who made the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Basis {
    /// `CSR_DREAMS_CONSENT`, set by the installer after its disclosure prompt.
    Installer,
    /// The person answered the question in this step.
    Asked,
    /// Nobody was asked: no tty and no preset. The documented default.
    Default,
    /// A decline recorded by an earlier setup, kept: an upgrade that answers the installer's
    /// yes, or a run with no one to ask, does not turn dreams back on.
    Kept,
}

/// A yes nobody gave at this step's own question (the installer's, or the silent default) does
/// not undo a decline an earlier setup recorded. Only the question asked here can.
pub(crate) fn keep_decline(plan: Plan, declined_before: bool) -> Plan {
    match plan {
        Plan::Decided(true, Basis::Installer | Basis::Default) if declined_before => {
            Plan::Decided(false, Basis::Kept)
        }
        other => other,
    }
}

/// One decision: the answer, who gave it, and whether the question has to be asked now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plan {
    Decided(bool, Basis),
    Ask,
}

/// The pure part: what to do given the preset and whether this run may prompt.
pub(crate) fn plan(preset: Preset, interactive: bool) -> Plan {
    match preset {
        Preset::Yes => Plan::Decided(true, Basis::Installer),
        Preset::No => Plan::Decided(false, Basis::Installer),
        Preset::Unset if interactive => Plan::Ask,
        Preset::Unset => Plan::Decided(true, Basis::Default),
    }
}

/// Does this decision close the card's question? Only an answer a person gave, and only a yes:
/// see the module doc for why a decline and a silent default leave the card in place.
pub(crate) fn closes_the_card(granted: bool, basis: Basis) -> bool {
    granted && matches!(basis, Basis::Installer | Basis::Asked)
}

/// Is this run allowed to ask an interactive question? `CSR_AUTO_SETUP=1`
/// (the installer's automation path) and a non-tty stdin both mean "take the
/// default without prompting". `CSR_SKIP_SETUP` belongs to `install.sh` and
/// is never reached here — if it were set, setup would not be running.
pub(crate) fn interactive() -> bool {
    if std::env::var("CSR_AUTO_SETUP")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
    {
        return false;
    }
    std::io::stdin().is_terminal()
}

/// Write the card's answer row for "on", unless a row is already there (a re-run must not
/// overwrite a choice made in the card). Returns whether it wrote one.
pub(crate) fn record_dreams_answer(store: &Store, granted: bool, now_ms: u64) -> Result<bool> {
    if store.get(MACHINE, CARD_ANSWER_KEY)?.is_some() {
        return Ok(false);
    }
    store.set(
        MACHINE,
        CARD_ANSWER_KEY,
        &json!({ "value": granted, "at": now_ms, "from": "setup" }),
    )?;
    Ok(true)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The estimate + toggle screen. Returns the recorded decision.
///
/// Honesty rules: the number shown is computed from the measured candidate
/// count in the corpus and the configured tier, it is labelled an estimate
/// and states its assumptions, and it is printed BEFORE the question. A
/// non-interactive run takes the documented default (ON) and says so rather
/// than silently assuming consent.
pub(super) fn dreaming_consent_step(storage: &Storage) -> Result<bool> {
    let preset = parse_preset(std::env::var("CSR_DREAMS_CONSENT").ok().as_deref());
    let card_store = Store::at(store::default_path());
    consent_step(
        storage,
        &card_store,
        keep_decline(
            plan(preset, interactive()),
            dream_cadence::consent_declined(storage),
        ),
        &mut ask_yes_default_yes,
    )
}

/// The effects of [`dreaming_consent_step`], with the store and the question passed in.
fn consent_step(
    storage: &Storage,
    card_store: &Store,
    plan: Plan,
    ask: &mut dyn FnMut(&str) -> bool,
) -> Result<bool> {
    let tier = policy::effort_tier();
    let estimate = night_estimate(storage, tier);

    eprintln!("[7/8] Dreaming (overnight review of your open work)");
    eprintln!("  While Claude Code is idle, CSR re-checks open todos and blockers");
    eprintln!("  against the code as it stands now, and writes what it can prove.");
    eprintln!("  Cost: {}", estimate.label(tier));
    eprintln!(
        "  Effort tier '{}' ({} reasoning, up to {} episodes per pass) — change with CSR_DREAM_EFFORT=less|balanced|max.",
        tier.as_str(),
        tier.reasoning_effort(),
        tier.episodes_per_pass(),
    );

    let (granted, basis) = match plan {
        Plan::Decided(granted, basis) => (granted, basis),
        Plan::Ask => (ask("  Enable dreaming? [Y/n] "), Basis::Asked),
    };
    match (granted, basis) {
        (true, Basis::Installer) => eprintln!(
            "  Dreaming: on (you agreed at the installer prompt). CSR_NO_DREAMING=1 turns it off; an installed daemon sees it only after setup runs again with it set."
        ),
        (false, Basis::Installer) => eprintln!(
            "  Dreaming: off (you declined at the installer prompt); the daemon will not run night passes."
        ),
        (_, Basis::Default) => {
            eprintln!("  Non-interactive run — keeping the default (enabled).")
        }
        (_, Basis::Kept) => eprintln!(
            "  Dreaming: stays off (you declined it before). Run `csr-engine setup` in a terminal to turn it on."
        ),
        _ => {}
    }

    dream_cadence::record_consent(storage, granted)?;
    if !matches!(basis, Basis::Installer | Basis::Kept) {
        if granted {
            eprintln!("  Dreaming enabled. Turn it off with CSR_NO_DREAMING=1; an installed daemon sees it only after setup runs again with it set.");
        } else {
            eprintln!("  Dreaming declined; the daemon will not run night passes.");
        }
    }
    if closes_the_card(granted, basis) {
        // The card's step is answered, so it does not ask again. Never fails setup.
        if let Err(error) = record_dreams_answer(card_store, granted, now_ms()) {
            eprintln!(
                "  Warning: could not record the answer for the in-session Dreams card: {error}"
            );
        }
    }
    Ok(granted)
}

/// Step 7's two questions: dreaming, then the pane's model limits.
pub(super) fn consent_steps(storage: &Storage) -> Result<()> {
    dreaming_consent_step(storage)?;
    if cfg!(target_os = "macos") {
        model_limits_step(
            &crate::companion::private::default_root(),
            interactive(),
            &mut ask_yes_default_no,
        );
    }
    Ok(())
}

/// Asks once whether the pane may show Claude's per-model weekly limits (Fable's share), which
/// means reading Claude Code's login token from the Keychain (`companion/claude_quota.rs`).
/// Off unless a person says yes here; an answer already on file is kept, never asked again.
/// Returns the answer now on file, if any. A switch that cannot be written never fails setup.
fn model_limits_step(
    keep: &std::path::Path,
    interactive: bool,
    ask: &mut dyn FnMut(&str) -> bool,
) -> Option<bool> {
    use crate::companion::claude_quota::SWITCH;
    let file = keep.join(SWITCH);
    eprintln!("      Model limits on the pane (optional)");
    if let Some(on) = crate::companion::paths::read_json(&file)["on"].as_bool() {
        eprintln!(
            "  Model limits: {} (answered before). Delete {} and run setup again to change it.",
            if on { "on" } else { "off" },
            file.display()
        );
        return Some(on);
    }
    if !interactive {
        eprintln!("  Model limits: off. Run `csr-engine setup` in a terminal to be asked.");
        return None;
    }
    eprintln!(
        "  The pane can show each Claude model's own weekly limit (Fable's share), as /usage does."
    );
    eprintln!("  To read it, CSR takes Claude Code's login token from your Keychain and asks");
    eprintln!("  Anthropic's usage endpoint every 5 minutes. The token is never stored or logged.");
    let on = ask("  Show model limits? [y/N] ");
    let answer = json!({ "on": on, "at": now_ms(), "from": "setup" });
    let written =
        std::fs::create_dir_all(keep).and_then(|()| std::fs::write(&file, answer.to_string()));
    match written {
        Ok(()) if on => eprintln!("  Model limits: on. CSR_NO_CLAUDE_QUOTA=1 turns them off."),
        Ok(()) => eprintln!("  Model limits: off."),
        Err(error) => eprintln!("  Warning: could not record the model limits answer: {error}"),
    }
    Some(on)
}

/// Read one line; only an explicit "y"/"yes" says yes.
fn ask_yes_default_no(prompt: &str) -> bool {
    eprint!("{prompt}");
    use std::io::Write;
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// The per-night estimate for `tier`, from the corpus's measured candidate
/// count and the configured cadence/budget.
fn night_estimate(storage: &Storage, tier: EffortTier) -> NightEstimate {
    policy::estimate_night(
        crate::dream::threads::count_candidate_episodes(storage),
        tier,
        policy::budget_cap(tier),
        dream_cadence::interval_secs(),
    )
}

/// Read one line; anything but an explicit "n"/"no" keeps the default (yes).
fn ask_yes_default_yes(prompt: &str) -> bool {
    eprint!("{prompt}");
    use std::io::Write;
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return true;
    }
    !matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempstore() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("csr-companion").join("companion.db"));
        (dir, store)
    }

    // ── what CSR_DREAMS_CONSENT means (pure) ──

    #[test]
    fn the_preset_reads_yes_no_and_ignores_anything_else() {
        for yes in ["yes", "YES", " Yes ", "1", "true", "True"] {
            assert_eq!(parse_preset(Some(yes)), Preset::Yes, "{yes:?}");
        }
        for no in ["no", "NO", " no\n", "0", "false", "False"] {
            assert_eq!(parse_preset(Some(no)), Preset::No, "{no:?}");
        }
        for other in ["", "  ", "maybe", "2", "y", "n"] {
            assert_eq!(parse_preset(Some(other)), Preset::Unset, "{other:?}");
        }
        assert_eq!(parse_preset(None), Preset::Unset);
    }

    #[test]
    fn the_decision_table() {
        // The installer's answer is final: no question, interactive or not.
        for interactive in [true, false] {
            assert_eq!(
                plan(Preset::Yes, interactive),
                Plan::Decided(true, Basis::Installer)
            );
            assert_eq!(
                plan(Preset::No, interactive),
                Plan::Decided(false, Basis::Installer)
            );
        }
        // Not set: ask when a person is there, else the documented default.
        assert_eq!(plan(Preset::Unset, true), Plan::Ask);
        assert_eq!(
            plan(Preset::Unset, false),
            Plan::Decided(true, Basis::Default)
        );
    }

    #[test]
    fn an_earlier_decline_survives_the_installers_yes_and_the_default_but_not_a_question() {
        let kept = Plan::Decided(false, Basis::Kept);
        assert_eq!(keep_decline(plan(Preset::Yes, true), true), kept);
        assert_eq!(keep_decline(plan(Preset::Unset, false), true), kept);
        assert_eq!(keep_decline(plan(Preset::Unset, true), true), Plan::Ask);
        assert_eq!(
            keep_decline(plan(Preset::No, false), true),
            Plan::Decided(false, Basis::Installer)
        );
        // With no decline on record nothing changes.
        for (preset, interactive) in [(Preset::Yes, true), (Preset::Unset, false)] {
            let p = plan(preset, interactive);
            assert_eq!(keep_decline(p, false), p);
        }
    }

    #[test]
    fn an_upgrade_through_the_installer_keeps_dreams_off_after_a_decline() {
        let storage = Storage::open_memory().unwrap();
        dream_cadence::record_consent(&storage, false).unwrap();
        let (_dir, store) = tempstore();
        let p = keep_decline(
            plan(Preset::Yes, false),
            dream_cadence::consent_declined(&storage),
        );
        let granted = consent_step(&storage, &store, p, &mut |_| panic!("asked")).unwrap();
        assert!(!granted);
        assert!(dream_cadence::consent_declined(&storage));
        assert_eq!(store.get(MACHINE, CARD_ANSWER_KEY).unwrap(), None);
    }

    #[test]
    fn only_a_yes_from_a_person_closes_the_card() {
        assert!(closes_the_card(true, Basis::Installer));
        assert!(closes_the_card(true, Basis::Asked));
        assert!(!closes_the_card(true, Basis::Default));
        assert!(!closes_the_card(false, Basis::Installer));
        assert!(!closes_the_card(false, Basis::Asked));
        assert!(!closes_the_card(false, Basis::Default));
    }

    // ── the machine row, against a Store in a tempdir ──

    #[test]
    fn the_answer_row_has_the_shape_the_card_writes() {
        let (_dir, store) = tempstore();
        assert!(record_dreams_answer(&store, true, 1_700_000_000_123).unwrap());
        let row = store.get(MACHINE, "setup:answer:dreams").unwrap().unwrap();
        assert_eq!(
            row,
            json!({ "value": true, "at": 1_700_000_000_123u64, "from": "setup" })
        );
    }

    #[test]
    fn a_rerun_does_not_overwrite_an_answer_already_there() {
        let (_dir, store) = tempstore();
        store
            .set(
                MACHINE,
                CARD_ANSWER_KEY,
                &json!({ "value": false, "at": 5 }),
            )
            .unwrap();
        assert!(!record_dreams_answer(&store, true, 9).unwrap());
        assert_eq!(
            store.get(MACHINE, CARD_ANSWER_KEY).unwrap().unwrap()["value"],
            false,
            "the card's Off survives a setup re-run"
        );
    }

    // ── the whole step: DB consent + machine row, no process env, no real HOME ──

    fn run_step(plan: Plan, answer: bool) -> (Storage, Store, tempfile::TempDir, usize) {
        let storage = Storage::open_memory().unwrap();
        let (dir, store) = tempstore();
        let mut asked = 0usize;
        consent_step(&storage, &store, plan, &mut |_| {
            asked += 1;
            answer
        })
        .unwrap();
        (storage, store, dir, asked)
    }

    fn consent_of(storage: &Storage) -> Option<String> {
        storage.get_meta(dream_cadence::META_CONSENT).unwrap()
    }

    #[test]
    fn installer_yes_grants_closes_the_card_and_never_asks() {
        let (storage, store, _dir, asked) = run_step(plan(Preset::Yes, true), false);
        assert_eq!(asked, 0);
        assert_eq!(
            consent_of(&storage).as_deref(),
            Some(dream_cadence::CONSENT_GRANTED)
        );
        assert!(!dream_cadence::consent_declined(&storage));
        let row = store.get(MACHINE, CARD_ANSWER_KEY).unwrap().unwrap();
        assert_eq!(row["value"], true);
        assert_eq!(row["from"], "setup");
    }

    #[test]
    fn installer_no_records_a_decline_without_asking_and_leaves_the_card() {
        let (storage, store, _dir, asked) = run_step(plan(Preset::No, true), true);
        assert_eq!(asked, 0);
        assert!(dream_cadence::consent_declined(&storage));
        assert_eq!(store.get(MACHINE, CARD_ANSWER_KEY).unwrap(), None);
    }

    #[test]
    fn an_interactive_yes_is_asked_once_granted_and_closes_the_card() {
        let (storage, store, _dir, asked) = run_step(plan(Preset::Unset, true), true);
        assert_eq!(asked, 1);
        assert!(!dream_cadence::consent_declined(&storage));
        assert!(store.get(MACHINE, CARD_ANSWER_KEY).unwrap().is_some());
    }

    #[test]
    fn an_interactive_no_declines_and_leaves_the_card() {
        let (storage, store, _dir, asked) = run_step(plan(Preset::Unset, true), false);
        assert_eq!(asked, 1);
        assert!(dream_cadence::consent_declined(&storage));
        assert_eq!(store.get(MACHINE, CARD_ANSWER_KEY).unwrap(), None);
    }

    #[test]
    fn a_silent_default_records_consent_for_the_dreamer_and_leaves_the_card() {
        let (storage, store, _dir, asked) = run_step(plan(Preset::Unset, false), false);
        assert_eq!(asked, 0);
        assert_eq!(
            consent_of(&storage).as_deref(),
            Some(dream_cadence::CONSENT_GRANTED),
            "a taken default is still recorded, not left ambiguous"
        );
        assert_eq!(store.get(MACHINE, CARD_ANSWER_KEY).unwrap(), None);
    }

    #[test]
    fn a_store_that_cannot_be_written_never_fails_the_step() {
        let storage = Storage::open_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // A file where the store's folder should be: opening the store fails.
        let blocker = dir.path().join("csr-companion");
        std::fs::write(&blocker, "not a folder").unwrap();
        let store = Store::at(blocker.join("companion.db"));
        let granted = consent_step(&storage, &store, plan(Preset::Yes, false), &mut |_| true);
        assert!(granted.unwrap(), "the decision still stands");
        assert!(!dream_cadence::consent_declined(&storage));
    }

    // ── the model limits question ──

    #[test]
    fn model_limits_are_off_unless_a_person_says_yes() {
        use crate::companion::claude_quota::{on, SWITCH};
        let dir = tempfile::tempdir().unwrap();
        let keep = dir.path().join("csr-companion");
        let never = &mut |_: &str| -> bool { panic!("asked") };
        assert_eq!(model_limits_step(&keep, false, never), None);
        assert!(!keep.join(SWITCH).exists(), "no one asked, nothing written");
        assert!(!on(&keep));

        let mut asked = 0;
        let got = model_limits_step(&keep, true, &mut |_| {
            asked += 1;
            false
        });
        assert_eq!((got, asked), (Some(false), 1));
        assert!(!on(&keep));
        assert_eq!(
            model_limits_step(&keep, true, never),
            Some(false),
            "an answer on file is not asked again"
        );

        std::fs::remove_file(keep.join(SWITCH)).unwrap();
        assert_eq!(model_limits_step(&keep, true, &mut |_| true), Some(true));
        assert!(on(&keep));
    }

    // ── what the step moved out of setup.rs ──

    /// Seed one candidate episode: a v2 reflection with a partial outcome and
    /// at least one touched file — exactly what a night pass would consider.
    fn seed_candidate(storage: &Storage, session: &str) {
        let content = serde_json::json!({
            "schema": "v2",
            "session_id": session,
            "project": "proj",
            "timestamp": "2026-08-11T10:00:00Z",
            "request": "do the thing",
            "outcome": "partial",
            "completed": "half of it",
            "files_modified": ["src/a.rs"],
        })
        .to_string();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO reflections (id, content, tags, timestamp)
                     VALUES (?1, ?2, '[]', '2026-08-11T10:00:00Z')",
                    rusqlite::params![session, content],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn the_estimate_is_computed_from_the_real_corpus_and_labelled_an_estimate() {
        let storage = Storage::open_memory().unwrap();
        let empty = night_estimate(&storage, EffortTier::Balanced);
        assert_eq!(empty.candidates, 0, "an empty corpus estimates from zero");
        assert_eq!(empty.total_tokens(), 0);

        for index in 0..3 {
            seed_candidate(&storage, &format!("session-{index}"));
        }
        let seeded = night_estimate(&storage, EffortTier::Balanced);
        assert_eq!(
            seeded.candidates, 3,
            "the estimate must count the corpus, not a constant"
        );
        assert!(seeded.total_tokens() > 0);
        let label = seeded.label(EffortTier::Balanced);
        assert!(label.contains("estimate"), "not labelled: {label}");
        assert!(label.contains("3 candidate episodes"), "no basis: {label}");
    }

    #[test]
    fn auto_setup_makes_the_run_non_interactive() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        std::env::set_var("CSR_AUTO_SETUP", "1");
        let auto = interactive();
        std::env::remove_var("CSR_AUTO_SETUP");
        assert!(!auto, "CSR_AUTO_SETUP=1 must never prompt");
    }

    #[test]
    fn a_declined_consent_stops_the_daemon_dreaming() {
        let storage = Storage::open_memory().unwrap();
        assert!(
            !dream_cadence::consent_declined(&storage),
            "never asked is not declined"
        );
        dream_cadence::record_consent(&storage, false).unwrap();
        assert!(dream_cadence::consent_declined(&storage));
        dream_cadence::record_consent(&storage, true).unwrap();
        assert!(!dream_cadence::consent_declined(&storage));
    }
}
