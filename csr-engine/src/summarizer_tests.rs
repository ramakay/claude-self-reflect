use super::*;

#[test]
fn test_build_prompt_with_full_context() {
    let ctx = StoryContext {
        current_session_excerpt: "user: fix the bug\nassistant: done".to_string(),
        current_v3_enrichment: Some("Tools: Edit | Files: main.rs".to_string()),
        related_sessions: vec![RelatedSession {
            summary: "Fixed similar bug in auth module".to_string(),
            age_label: "2d ago".to_string(),
            score: 0.72,
        }],
        project_name: "my-project".to_string(),
    };
    let prompt = build_prompt(&ctx);
    assert!(prompt.contains("CURRENT SESSION TRANSCRIPT"));
    assert!(prompt.contains("fix the bug"));
    assert!(prompt.contains("STRUCTURED ANALYSIS"));
    assert!(prompt.contains("Tools: Edit"));
    assert!(prompt.contains("RELATED PAST SESSIONS"));
    assert!(prompt.contains("72%"));
    assert!(prompt.contains("2-3 sentences"));
}

#[test]
fn test_build_prompt_minimal() {
    let ctx = StoryContext {
        current_session_excerpt: "user: hello".to_string(),
        current_v3_enrichment: None,
        related_sessions: vec![],
        project_name: "test".to_string(),
    };
    let prompt = build_prompt(&ctx);
    assert!(prompt.contains("hello"));
    assert!(!prompt.contains("STRUCTURED ANALYSIS"));
    assert!(!prompt.contains("RELATED PAST SESSIONS"));
}

fn prior(rev: usize, covered: Option<usize>) -> PriorStory {
    PriorStory {
        rev,
        covered,
        ..PriorStory::default()
    }
}

#[test]
fn a_first_story_is_written_once() {
    assert_eq!(story_due(StoryMode::First, None, 6, None, 0), Ok(()));
    assert_eq!(
        story_due(StoryMode::First, Some(&prior(1, None)), 4736, None, 3),
        Err("skip:story_exists".into())
    );
}

#[test]
fn every_compaction_retells_the_story_and_nothing_caps_it() {
    let mode = StoryMode::FromCompaction;
    assert_eq!(
        story_due(mode, None, 900, None, 0),
        Err("skip:no_compaction_summary".into())
    );
    assert_eq!(story_due(mode, None, 900, Some(880), 0), Ok(()));
    // a story from before the count was kept has not taken any summary in
    assert_eq!(
        story_due(mode, Some(&prior(1, None)), 900, Some(880), 0),
        Ok(())
    );
    // the twelfth compaction is told like the second
    assert_eq!(
        story_due(mode, Some(&prior(11, Some(4200))), 4736, Some(4700), 0),
        Ok(())
    );
    // the same summary is never told twice (a resumed session, a retry)
    assert_eq!(
        story_due(mode, Some(&prior(12, Some(4736))), 4736, Some(4700), 0),
        Err("skip:summary_told(4700)".into())
    );
}

#[test]
fn haiku_retells_the_ending_only_when_the_person_asked_something_since() {
    let mode = StoryMode::Rewrite;
    assert_eq!(story_due(mode, None, 900, None, 0), Ok(()));
    // 4736 of 4800 told, but a new ask came after: the ending is due, however small a share
    assert_eq!(
        story_due(mode, Some(&prior(12, Some(4736))), 4800, None, 1),
        Ok(())
    );
    // the agent worked on, nothing new was asked: the story stands
    assert_eq!(
        story_due(mode, Some(&prior(12, Some(4736))), 4800, None, 0),
        Err("skip:nothing_asked_since(4736/4800)".into())
    );
    // nothing after the last message, but something asked: due all the same
    assert_eq!(
        story_due(mode, Some(&prior(3, Some(4800))), 4800, None, 2),
        Ok(())
    );
    assert_eq!(
        story_due(mode, Some(&prior(3, Some(4800))), 4800, None, 0),
        Err("skip:nothing_asked_since(4800/4800)".into())
    );
    assert_eq!(
        story_due(mode, Some(&prior(1, None)), 4000, None, 0),
        Ok(())
    );
}

#[test]
fn asked_since_counts_only_what_came_after_the_story() {
    let user = |text: &str| serde_json::json!({"type": "user", "message": {"role": "user", "content": text}});
    let messages = vec![user("old ask"), user("told ask"), user("new ask")];
    assert_eq!(asked_since(&messages, &[], None), 3);
    assert_eq!(asked_since(&messages, &[], Some(&prior(1, Some(2)))), 1);
    assert_eq!(asked_since(&messages, &[], Some(&prior(1, Some(9)))), 0);
}

/// A transcript with `told` user/assistant rows, then `tail` raw lines.
fn transcript_with(told: usize, tail: &[serde_json::Value]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut body = String::new();
    for n in 0..told {
        let row = if n % 2 == 0 {
            serde_json::json!({"type": "user", "message": {"role": "user", "content": format!("ask {n}")}})
        } else {
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": "done"}})
        };
        body.push_str(&format!("{row}\n"));
    }
    for row in tail {
        body.push_str(&format!("{row}\n"));
    }
    std::fs::write(dir.path().join("s.jsonl"), body).unwrap();
    dir
}

/// Is a rewrite due for a story told when the last assistant row had not come yet?
fn rewrite_due(dir: &tempfile::TempDir, told: usize) -> Result<(), String> {
    let (messages, queued) =
        crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
            .unwrap();
    let prior = prior(1, Some(told - 1));
    story_due(
        StoryMode::Rewrite,
        Some(&prior),
        messages.len(),
        None,
        asked_since(&messages, &queued, Some(&prior)),
    )
}

fn queued_row(prompt: serde_json::Value, human: bool, at: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "attachment", "timestamp": at,
        "attachment": {"type": "queued_command", "prompt": prompt, "humanTurn": human}
    })
}

#[test]
fn a_prompt_typed_while_claude_was_busy_makes_a_rewrite_due() {
    let dir = transcript_with(
        6,
        &[queued_row(
            serde_json::json!("also fix the pane"),
            true,
            "2026-10-14T20:59:56Z",
        )],
    );
    assert_eq!(rewrite_due(&dir, 6), Ok(()));
    // The same prompt as text blocks.
    let dir = transcript_with(
        6,
        &[queued_row(
            serde_json::json!([{"type": "text", "text": "also fix the pane"}]),
            true,
            "2026-10-14T20:59:56Z",
        )],
    );
    assert_eq!(rewrite_due(&dir, 6), Ok(()));
}

#[test]
fn a_queued_command_nobody_typed_still_skips() {
    let dir = transcript_with(
        6,
        &[
            queued_row(
                serde_json::json!("<task-notification>build finished</task-notification>"),
                false,
                "2026-10-14T20:59:56Z",
            ),
            queued_row(
                serde_json::json!("not flagged"),
                false,
                "2026-10-14T21:00:00Z",
            ),
        ],
    );
    assert_eq!(
        rewrite_due(&dir, 6),
        Err("skip:nothing_asked_since(5/6)".to_string())
    );
}

#[test]
fn a_queued_prompt_that_also_arrives_as_a_user_row_counts_once() {
    let typed = "also fix the pane";
    let queued = queued_row(serde_json::json!(typed), true, "2026-10-14T20:59:56Z");
    let twice = queued_row(serde_json::json!(typed), true, "2026-10-14T20:59:56Z");
    let row = serde_json::json!({"type": "user", "message": {"role": "user", "content": typed}});
    let dir = transcript_with(6, &[queued, twice, row]);
    let (messages, queued) =
        crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
            .unwrap();
    assert_eq!(queued.len(), 1, "one enqueue written twice");
    let prior = prior(1, Some(6));
    assert_eq!(asked_since(&messages, &queued, Some(&prior)), 1);
    // and a queued prompt before the story's coverage is not new
    let before = prior_covering_all(&messages);
    assert_eq!(asked_since(&messages, &queued, Some(&before)), 0);
}

/// Indexing queued prompts adds chunk rows of their own; it must not move what the
/// story reads. The messages the summarizer gets carry no queued prompt, so a stored
/// `covered` count still indexes into them, and `asked_since` counts as it did.
#[test]
fn queued_prompts_leave_the_storys_messages_and_counts_alone() {
    let row = |n: usize| {
        if n.is_multiple_of(2) {
            serde_json::json!({"type": "user", "message": {"role": "user", "content": format!("ask {n}")}})
        } else {
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": "done"}})
        }
    };
    let write = |name: &str, rows: &[serde_json::Value]| {
        let dir = tempfile::tempdir().unwrap();
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(dir.path().join(name), body).unwrap();
        dir
    };
    let alpha = queued_row(
        serde_json::json!("alpha while busy"),
        true,
        "2026-10-14T20:00:00Z",
    );
    let beta = queued_row(
        serde_json::json!("beta while busy"),
        true,
        "2026-10-14T20:01:00Z",
    );
    let plain = [row(0), row(1), row(2), row(3), row(4), row(5)];
    let with_queued = [
        row(0),
        row(1),
        row(2),
        alpha.clone(),
        row(3),
        row(4),
        row(5),
        beta.clone(),
    ];
    let parse = |dir: &tempfile::TempDir| {
        crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
            .unwrap()
    };
    let without = write("s.jsonl", &plain);
    let with = write("s.jsonl", &with_queued);
    let (plain_messages, plain_queued) = parse(&without);
    let (messages, queued) = parse(&with);
    assert_eq!(messages.len(), 6);
    assert_eq!(
        messages, plain_messages,
        "no queued prompt inside the messages"
    );
    assert!(plain_queued.is_empty());
    assert_eq!(
        queued
            .iter()
            .map(|q| (q.place, q.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(3, "alpha while busy"), (6, "beta while busy")]
    );
    // A story told over the first four messages: one user row after it, the
    // prompt queued inside its coverage was told, the one after the end is new.
    let told = prior(1, Some(4));
    assert_eq!(asked_since(&plain_messages, &[], Some(&told)), 1);
    assert_eq!(asked_since(&messages, &queued, Some(&told)), 2);
}

/// Is a rewrite due for a story that covers every message the transcript has?
fn rewrite_due_covering_all(dir: &tempfile::TempDir) -> Result<(), String> {
    let (messages, queued) =
        crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
            .unwrap();
    let prior = prior_covering_all(&messages);
    story_due(
        StoryMode::Rewrite,
        Some(&prior),
        messages.len(),
        None,
        asked_since(&messages, &queued, Some(&prior)),
    )
}

#[test]
fn a_prompt_queued_after_the_last_message_makes_a_story_covering_everything_due() {
    let human = transcript_with(
        6,
        &[queued_row(
            serde_json::json!("also fix the pane"),
            true,
            "2026-10-14T20:59:56Z",
        )],
    );
    assert_eq!(rewrite_due_covering_all(&human), Ok(()));
    // and nothing typed still skips
    let robot = transcript_with(
        6,
        &[queued_row(
            serde_json::json!("<task-notification>done</task-notification>"),
            false,
            "2026-10-14T20:59:56Z",
        )],
    );
    assert_eq!(
        rewrite_due_covering_all(&robot),
        Err("skip:nothing_asked_since(6/6)".to_string())
    );
}

#[test]
fn a_queued_prompt_holding_a_secret_counts_once_with_its_scrubbed_user_row() {
    let typed = format!("rotate this key {}{}", "ghp_", "Q".repeat(36));
    let queued = queued_row(serde_json::json!(typed), true, "2026-10-14T20:59:56Z");
    let row = serde_json::json!({"type": "user", "message": {"role": "user", "content": typed}});
    let dir = transcript_with(6, &[queued, row]);
    let (messages, queued) =
        crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
            .unwrap();
    assert_eq!(queued.len(), 1);
    assert!(
        !queued[0].text.contains("ghp_"),
        "the queued text is scrubbed like the row: {}",
        queued[0].text
    );
    let prior = prior(1, Some(6));
    assert_eq!(asked_since(&messages, &queued, Some(&prior)), 1);
}

#[test]
fn a_prompt_queued_inside_the_story_is_not_asked_again_when_its_user_row_lands_after_it() {
    let typed = "also fix the pane";
    let assistant = |text: &str| serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": text}});
    // Queued after four messages; two assistant rows follow; the story covers six;
    // only then does the prompt arrive as a user row.
    let dir = transcript_with(
        4,
        &[
            queued_row(serde_json::json!(typed), true, "2026-10-14T20:59:56Z"),
            assistant("one"),
            assistant("two"),
            serde_json::json!({"type": "user", "message": {"role": "user", "content": typed}}),
        ],
    );
    let (messages, queued) =
        crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
            .unwrap();
    assert_eq!((messages.len(), queued[0].place), (7, 4));
    let told = prior(1, Some(6));
    assert_eq!(asked_since(&messages, &queued, Some(&told)), 0);
    // The same text typed again, with no queued twin, is a new ask.
    let again = serde_json::json!({"type": "user", "message": {"role": "user", "content": typed}});
    let mut more = messages.clone();
    more.push(again);
    assert_eq!(asked_since(&more, &queued, Some(&told)), 1);
    // And a queued prompt whose row came before the edge leaves a later copy new.
    let early = prior(1, Some(7));
    assert_eq!(asked_since(&messages, &queued, Some(&early)), 0);
    assert_eq!(asked_since(&more, &queued, Some(&early)), 1);
}

fn written(covered: usize, at: &str) -> PriorStory {
    PriorStory {
        written_at: crate::temporal::parse_timestamp(at),
        ..prior(1, Some(covered))
    }
}

fn parsed(dir: &tempfile::TempDir) -> (Vec<serde_json::Value>, Vec<crate::import::QueuedPrompt>) {
    crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl")).unwrap()
}

/// A rewrite that has nothing new to cover leaves `covered` where it was. The prompt that
/// was queued after the last message is told by then, and must not count again.
#[test]
fn a_queued_prompt_the_story_was_written_after_is_not_asked_again() {
    let dir = transcript_with(
        6,
        &[queued_row(
            serde_json::json!("also fix the pane"),
            true,
            "2026-10-14T20:59:56Z",
        )],
    );
    let (messages, queued) = parsed(&dir);
    assert_eq!(queued[0].place, 6);
    // First rewrite: the story is older than the prompt, so it is a new ask.
    let before = written(6, "2026-10-14T20:00:00Z");
    assert_eq!(asked_since(&messages, &queued, Some(&before)), 1);
    // After the rewrite: covered is still 6, but the story postdates the prompt.
    let after = written(6, "2026-10-14T21:05:00Z");
    assert_eq!(asked_since(&messages, &queued, Some(&after)), 0);
    assert_eq!(
        story_due(
            StoryMode::Rewrite,
            Some(&after),
            messages.len(),
            None,
            asked_since(&messages, &queued, Some(&after)),
        ),
        Err("skip:nothing_asked_since(6/6)".to_string())
    );
    // A story whose time is unknown cannot rule the prompt out.
    assert_eq!(asked_since(&messages, &queued, Some(&prior(1, Some(6)))), 1);
    // Nor can a prompt whose row carries no time.
    let undated = transcript_with(
        6,
        &[queued_row(
            serde_json::json!("also fix the pane"),
            true,
            "not a time",
        )],
    );
    let (messages, queued) = parsed(&undated);
    assert_eq!(queued[0].at, None);
    assert_eq!(asked_since(&messages, &queued, Some(&after)), 1);
    // A prompt typed after the story was written is new.
    let newer = transcript_with(
        6,
        &[queued_row(
            serde_json::json!("and one more thing"),
            true,
            "2026-10-14T21:10:00Z",
        )],
    );
    let (messages, queued) = parsed(&newer);
    assert_eq!(asked_since(&messages, &queued, Some(&after)), 1);
}

/// The story is generated from a snapshot, then waits on a lock and a model call before it
/// is stored. A prompt typed in that window is after the snapshot's newest line, so the
/// next rewrite check still sees it as new, though it landed before the story was stored.
#[test]
fn a_prompt_typed_while_the_story_was_generated_is_still_a_new_ask() {
    let stamped = |n: usize, at: &str| {
        let role = if n.is_multiple_of(2) {
            "user"
        } else {
            "assistant"
        };
        serde_json::json!({
            "type": role, "timestamp": at,
            "message": {"role": role, "content": format!("line {n}")}
        })
    };
    let snapshot_dir = tempfile::tempdir().unwrap();
    let mut body = String::new();
    for n in 0..6 {
        body.push_str(&format!("{}\n", stamped(n, "2026-01-01T10:00:00Z")));
    }
    std::fs::write(snapshot_dir.path().join("s.jsonl"), &body).unwrap();
    let (messages, queued) = parsed(&snapshot_dir);
    let cutoff = snapshot_cutoff(&messages, &queued);
    assert_eq!(
        cutoff,
        crate::temporal::parse_timestamp("2026-01-01T10:00:00Z")
    );

    // The story is stored now (long after the snapshot), carrying the snapshot's cutoff.
    let storage = Storage::open_memory().unwrap();
    let tags = vec!["session_story".to_string(), "conv_conv-race".to_string()];
    storage
        .insert_reflection("story", "the story", &tags, &[0.0; 384])
        .unwrap();
    storage
        .mark_story_written("conv-race", "story_conv-race", messages.len(), cutoff)
        .unwrap();
    let prior = prior_story(&storage, "conv-race").unwrap();
    assert_eq!(prior.told_through, cutoff);

    // Typed after the snapshot, before the story was stored.
    let mut later = body.clone();
    later.push_str(&format!(
        "{}\n",
        queued_row(
            serde_json::json!("typed during generation"),
            true,
            "2026-01-01T10:05:00Z",
        )
    ));
    std::fs::write(snapshot_dir.path().join("s.jsonl"), later).unwrap();
    let (messages, queued) = parsed(&snapshot_dir);
    assert!(prior.written_at.unwrap() > queued[0].at.unwrap());
    assert_eq!(asked_since(&messages, &queued, Some(&prior)), 1);
    assert_eq!(
        story_due(
            StoryMode::Rewrite,
            Some(&prior),
            messages.len(),
            None,
            asked_since(&messages, &queued, Some(&prior)),
        ),
        Ok(())
    );
    // A story from before the cutoff was kept falls back to its write time.
    let legacy = PriorStory {
        told_through: None,
        ..prior
    };
    assert_eq!(asked_since(&messages, &queued, Some(&legacy)), 0);
}

/// The story told the queued prompt; its user row landing after the edge is that same
/// prompt, not a new ask.
#[test]
fn the_user_row_of_a_prompt_the_story_told_by_time_is_not_asked_again() {
    let typed = "also fix the pane";
    let dir = transcript_with(
        6,
        &[
            queued_row(serde_json::json!(typed), true, "2026-10-14T20:59:56Z"),
            serde_json::json!({"type": "user", "message": {"role": "user", "content": typed}}),
        ],
    );
    let (messages, queued) = parsed(&dir);
    assert_eq!(messages.len(), 7);
    assert_eq!(
        asked_since(
            &messages,
            &queued,
            Some(&written(6, "2026-10-14T21:05:00Z"))
        ),
        0
    );
    // Written before the prompt was typed, the row is the prompt arriving: one ask.
    assert_eq!(
        asked_since(
            &messages,
            &queued,
            Some(&written(6, "2026-10-14T20:00:00Z"))
        ),
        1
    );
}

#[test]
fn the_story_a_session_has_remembers_when_it_was_written() {
    let storage = Storage::open_memory().unwrap();
    let tags = vec!["session_story".to_string(), "conv_conv-w".to_string()];
    storage
        .insert_reflection("story", "the story", &tags, &[0.0; 384])
        .unwrap();
    let found = prior_story(&storage, "conv-w").unwrap();
    let written = found.written_at.expect("the reflection has a time");
    assert!(
        (chrono::Utc::now() - written).num_seconds().abs() < 60,
        "{written}"
    );
}

/// Haiku reads the prompt typed while Claude was busy, where it was typed. The messages
/// the numbering rests on are the same as before.
#[test]
fn the_story_prompt_shows_prompts_typed_while_claude_was_busy() {
    let dir = transcript_with(
        4,
        &[
            queued_row(
                serde_json::json!("alpha typed mid turn"),
                true,
                "2026-10-14T20:00:00Z",
            ),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": "working"}}),
            // This one reaches the transcript as a user row too: shown once.
            queued_row(
                serde_json::json!("beta typed mid turn"),
                true,
                "2026-10-14T20:01:00Z",
            ),
            serde_json::json!({"type": "user", "message": {"role": "user", "content": "beta typed mid turn"}}),
            // A harness notice is not the person.
            queued_row(
                serde_json::json!("<task-notification>gamma</task-notification>"),
                false,
                "2026-10-14T20:02:00Z",
            ),
            // Queued after the last message.
            queued_row(
                serde_json::json!("delta typed at the end"),
                true,
                "2026-10-14T20:03:00Z",
            ),
        ],
    );
    let (messages, queued) = parsed(&dir);
    assert_eq!(messages.len(), 6);
    let before = messages.clone();

    let excerpt = story_excerpt(&messages, &queued);
    assert_eq!(messages, before, "the messages are not touched");
    assert_eq!(excerpt.matches("alpha typed mid turn").count(), 1);
    assert_eq!(excerpt.matches("beta typed mid turn").count(), 1);
    assert_eq!(excerpt.matches("delta typed at the end").count(), 1);
    assert!(!excerpt.contains("gamma"));
    let position = |needle: &str| excerpt.find(needle).unwrap();
    // alpha sits after the fourth message and before the assistant row that follows it.
    assert!(position("ask 2") < position("alpha"));
    assert!(position("alpha") < position("working"));
    assert!(position("beta") > position("working"));
    assert!(position("delta") > position("beta"));
    assert!(
        excerpt.contains("[user]: ") && excerpt.contains("alpha"),
        "shown as a user line: {excerpt}"
    );

    // The summary path reads a range of the messages: the same prompts, by place.
    let tail = story_tail(&messages, &queued, 4..messages.len());
    assert!(tail.contains("alpha") && tail.contains("delta"), "{tail}");
    assert!(!tail.contains("ask 2"));
    let prompt = summary_story_prompt("the summary", &tail);
    assert!(prompt.contains("alpha typed mid turn"));
    // A range that stops short of a prompt's place leaves it out.
    let head = story_tail(&messages, &queued, 0..4);
    assert!(!head.contains("alpha") && !head.contains("delta"), "{head}");
}

fn prior_covering_all(messages: &[serde_json::Value]) -> PriorStory {
    prior(1, Some(messages.len()))
}

#[test]
fn a_haiku_story_reads_the_summary_and_the_messages_it_never_saw() {
    let prompt = summary_story_prompt(
        "1. Primary Request and Intent: build the reel",
        "[user]: ship v8",
    );
    let at = |needle: &str| prompt.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(at("build the reel") < at("THE LAST MESSAGES"));
    assert!(at("THE LAST MESSAGES") < at("ship v8"));
    assert!(prompt.contains("trust the last messages"));
    assert!(prompt.contains("under 800 characters"));
}

const SUMMARY: &str = "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.

Summary:
1. Primary Request and Intent:
   - The user asked to **refresh** the session story at each compaction in `summarizer.rs`. They want it smart, not capped.
   - A second ask about the pane.

2. Key Technical Concepts:
   - enrichment_state holds coverage.

8. Current Work:
   I was wiring the `--from-compaction` mode into SessionStart. Tests pass. I ran them twice.

9. Optional Next Step:
   Install the binary.
";

#[test]
fn a_compaction_summary_becomes_what_was_asked_and_where_it_stood() {
    let (story, templated) = story_from_compaction_summary(SUMMARY).unwrap();
    assert!(templated);
    assert_eq!(
        story,
        "The user asked to refresh the session story at each compaction in summarizer.rs. They want it smart, not capped. A second ask about the pane. Where it stood: Claude was wiring the --from-compaction mode into SessionStart. Tests pass. Claude ran them twice."
    );
}

#[test]
fn a_summary_without_the_template_is_read_from_its_start() {
    let (story, templated) = story_from_compaction_summary(
        "Summary:\nWe built the **feed**. It works. "
            .repeat(80)
            .as_str(),
    )
    .unwrap();
    assert!(!templated);
    assert!(story.starts_with("We built the feed. It works."));
    assert!(story.chars().count() <= ASKED_STORY_CHARS + STOOD_STORY_CHARS);
    // a first sentence longer than the room is cut, not dropped
    let long = format!("Summary:\n{}", "word ".repeat(400));
    let (story, _) = story_from_compaction_summary(&long).unwrap();
    assert!(story.ends_with('…'));
}

#[test]
fn the_newest_compaction_summary_is_found_with_its_place() {
    let row = |text: &str, summary: bool| {
        let mut row =
            serde_json::json!({"type": "user", "message": {"role": "user", "content": text}});
        if summary {
            row["isCompactSummary"] = true.into();
        }
        row
    };
    let messages = vec![
        row("ask", false),
        row("old summary", true),
        row("ask again", false),
        row("new summary", true),
        row("after", false),
    ];
    assert_eq!(
        newest_compaction_summary(&messages),
        Some((3, "new summary".to_string()))
    );
    assert_eq!(newest_compaction_summary(&messages[..1]), None);
}

#[test]
fn only_what_the_person_typed_counts_as_asked() {
    let user = |content: serde_json::Value| serde_json::json!({"type": "user", "message": {"role": "user", "content": content}});
    let mut summary = user(serde_json::json!(
        "This session is being continued from a previous conversation"
    ));
    summary["isCompactSummary"] = true.into();
    let mut meta = user(serde_json::json!(
        "<local-command-caveat>caveat</local-command-caveat>"
    ));
    meta["isMeta"] = true.into();
    let messages = vec![
        user(serde_json::json!("first   ask\n with  spaces")),
        summary,
        meta,
        user(
            serde_json::json!([{"type": "tool_result", "tool_use_id": "t", "content": "cargo test ok"}]),
        ),
        serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": "done"}}),
        user(serde_json::json!([{"type": "text", "text": "second ask"}])),
        user(serde_json::json!("third ask is long enough to cut")),
    ];
    assert_eq!(
        typed_prompts(&messages, 10, 15),
        vec!["first ask with", "second ask", "third ask is lo"]
    );
    // the newest ones when there are more than it reads
    assert_eq!(
        typed_prompts(&messages, 1, 300),
        vec!["third ask is long enough to cut"]
    );
}

#[test]
fn the_story_a_session_has_is_its_newest_with_the_highest_number() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::open(&tmp.path().join("csr.db")).unwrap();
    assert!(prior_story(&storage, "conv-a").is_none());
    let tags = |extra: &[&str]| {
        let mut tags = vec!["session_story".to_string(), "conv_conv-a".to_string()];
        tags.extend(extra.iter().map(|t| t.to_string()));
        tags
    };
    storage
        .insert_reflection("old", "first story", &tags(&[]), &[0.0; 384])
        .unwrap();
    storage
        .insert_reflection(
            "other",
            "another session",
            &["session_story".into(), "conv_conv-b".into()],
            &[0.0; 384],
        )
        .unwrap();
    let found = prior_story(&storage, "conv-a").unwrap();
    assert_eq!((found.rev, found.covered, found.ids.len()), (1, None, 1));
    std::thread::sleep(std::time::Duration::from_millis(5));
    storage
        .insert_reflection("new", "second story", &tags(&["story_rev_2"]), &[0.0; 384])
        .unwrap();
    storage
        .mark_story_written("conv-a", "story_conv-a", 640, None)
        .unwrap();
    let found = prior_story(&storage, "conv-a").unwrap();
    assert_eq!((found.rev, found.covered), (2, Some(640)));
    let mut ids = found.ids.clone();
    ids.sort();
    assert_eq!(ids, vec!["new", "old"]);
}

#[test]
fn a_story_rewrite_never_finds_ledger_rows_in_prior_story() {
    let storage = Storage::open_memory().unwrap();
    storage
        .insert_reflection(
            "story-1",
            "session story prose",
            &[
                "session_story".to_string(),
                "conv_c1".to_string(),
                "story_rev_1".to_string(),
            ],
            &[0.0f32; 384],
        )
        .unwrap();
    for (id, sig) in [
        ("ledger-1", "1111111111111111"),
        ("ledger-2", "2222222222222222"),
    ] {
        storage
            .insert_reflection(
                id,
                "[shift: model-derived, kind=shift, rev=1, receipts=evo:1] a => b - claim",
                &[
                    "narrative_shift".to_string(),
                    "shift_project:csr".to_string(),
                    "shift_conv:c1".to_string(),
                    "shift_rev_1".to_string(),
                    "shift_kind_shift".to_string(),
                    format!("shift_sig_{sig}"),
                    "shift_grounded".to_string(),
                ],
                &[0.0f32; 384],
            )
            .unwrap();
    }
    let found = prior_story(&storage, "c1").expect("story exists");
    assert_eq!(found.ids, vec!["story-1".to_string()]);
}

#[test]
fn a_story_rewrite_replaces_the_story_row_and_keeps_every_ledger_row() {
    let _guard = crate::story_map::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("CSR_NO_STORY_MAP");

    let storage = std::sync::Arc::new(Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(crate::search::SearchEngine::new(
        100,
    )));
    let engine = Engine::from_parts(
        storage.clone(),
        embeddings,
        search,
        std::path::PathBuf::from("/tmp"),
    );

    storage
        .with_connection(|c| {
            c.execute(
                "INSERT INTO code_evolution (
                        id, session_id, project_name, file_path, language, timestamp,
                        functions_added, functions_removed, types_added, types_removed
                    ) VALUES
                    ('evo_1', 'conv-keep-ledger', 'csr', '/repo/src/a.rs', 'rust', '2026-10-14T00:00:01Z', '[\"f1\"]', '[]', '[]', '[]'),
                    ('evo_2', 'conv-keep-ledger', 'csr', '/repo/src/b.rs', 'rust', '2026-10-14T00:00:02Z', '[\"f2\"]', '[]', '[]', '[]')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let s1 = crate::story_map::parse_shift_line("shift|-|a.rs:f1+|Added f1").unwrap();
    let s2 = crate::story_map::parse_shift_line("shift|-|b.rs:f2+|Added f2").unwrap();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        store_story(
            &engine,
            "/repo/csr",
            "conv-keep-ledger",
            "csr",
            "First session story.",
            None,
            10,
            None,
            vec!["story_by_haiku".to_string()],
        )
        .await
        .unwrap();
        crate::story_map::store::record_story_map(
            &engine,
            "csr",
            "conv-keep-ledger",
            1,
            10,
            "Haiku",
            vec![s1, s2],
        )
        .await;

        let ledger_before: Vec<String> = storage
            .get_ledger_reflections_by_tag("narrative_shift", 10)
            .unwrap()
            .into_iter()
            .map(|(id, _, _, _)| id)
            .collect();
        assert_eq!(ledger_before.len(), 2);

        let prior = prior_story(&storage, "conv-keep-ledger");
        assert!(prior.is_some());
        store_story(
            &engine,
            "/repo/csr",
            "conv-keep-ledger",
            "csr",
            "Second rewritten session story.",
            prior.as_ref(),
            25,
            None,
            vec!["story_by_haiku".to_string()],
        )
        .await
        .unwrap();

        let stories = storage
            .get_reflections_by_two_tags("session_story", "conv_conv-keep-ledger", 10)
            .unwrap();
        assert_eq!(stories.len(), 1);
        assert_eq!(stories[0].1, "Second rewritten session story.");

        let ledger_after: Vec<String> = storage
            .get_ledger_reflections_by_tag("narrative_shift", 10)
            .unwrap()
            .into_iter()
            .map(|(id, _, _, _)| id)
            .collect();
        assert_eq!(ledger_after, ledger_before);
    });
}

/// A canned model reply, no `claude -p`: the prompt asks for five fields, the reply is
/// parsed, grounded and stored, and the stored map and the shift reflection carry the
/// Before and After sentences. A 4-field line in the same reply still lands.
#[test]
fn a_canned_five_field_reply_is_stored_with_before_and_after() {
    let _guard = crate::story_map::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("CSR_NO_STORY_MAP");

    let prompt = build_prompt(&StoryContext {
        current_session_excerpt: String::new(),
        current_v3_enrichment: None,
        related_sessions: Vec::new(),
        project_name: "csr".into(),
    });
    assert!(prompt.contains("kind|before|after|was|now"), "{prompt}");
    assert!(summary_story_prompt("s", "t").contains("kind|before|after|was|now"));

    let storage = std::sync::Arc::new(Storage::open_memory().unwrap());
    let engine = Engine::from_parts(
        storage.clone(),
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(crate::search::SearchEngine::new(
            100,
        ))),
        std::path::PathBuf::from("/tmp"),
    );
    storage
        .with_connection(|c| {
            c.execute(
                "INSERT INTO code_evolution (
                        id, session_id, project_name, file_path, language, timestamp,
                        functions_added, functions_removed, types_added, types_removed
                    ) VALUES
                    ('evo_1', 'conv-canned', 'csr', '/repo/src/a.rs', 'rust', '2026-10-14T00:00:01Z', '[\"f1\"]', '[]', '[]', '[]'),
                    ('evo_2', 'conv-canned', 'csr', '/repo/src/b.rs', 'rust', '2026-10-14T00:00:02Z', '[\"f2\"]', '[]', '[]', '[]')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let reply = "The session moved the map to text.\n@shifts\n\
                     shift|-|a.rs:f1+|-|The map writes Before and After as text.\n\
                     shift|-|b.rs:f2+|The map drew its labels at 9 px.|The map writes labels as text rows.\n";
    let (prose, shifts) = crate::story_map::finish_story_output(reply, true);
    assert_eq!(prose, "The session moved the map to text.");
    assert_eq!(shifts.len(), 2);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(crate::story_map::store::record_story_map(
        &engine,
        "csr",
        "conv-canned",
        1,
        10,
        "Haiku",
        shifts,
    ));

    let map = crate::story_map::store::load_story_map(&storage, "conv-canned").expect("map");
    let entries = &map.current.entries;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].was, None);
    assert_eq!(
        entries[0].now.as_deref(),
        Some("The map writes Before and After as text.")
    );
    assert_eq!(
        entries[1].was.as_deref(),
        Some("The map drew its labels at 9 px.")
    );
    let said: Vec<String> = storage
        .get_ledger_reflections_by_tag("narrative_shift", 10)
        .unwrap()
        .into_iter()
        .map(|(_, content, _, _)| content)
        .collect();
    assert!(said
        .iter()
        .any(|c| c.ends_with("- New: The map writes Before and After as text.")));
    assert!(said.iter().any(|c| c.ends_with(
        "- Before: The map drew its labels at 9 px. After: The map writes labels as text rows."
    )));
}

#[test]
fn the_v3_lookup_never_takes_a_shift_ledger_row_for_an_analysis() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let tag = |t: &str| t.to_string();
    storage
        .insert_reflection(
            "v3",
            "Tools: Edit | Files: main.rs",
            &[tag("project_csr"), tag("narrative_extracted_v3")],
            &[0.0f32; 384],
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    storage
        .insert_reflection(
            "shift",
            "[shift: model-derived, kind=risk, rev=1, receipts=evo:1] a => b - claim",
            &[tag("project_csr"), tag("narrative_shift")],
            &[0.0f32; 384],
        )
        .unwrap();
    let got = get_latest_v3_enrichment(&storage, "csr").unwrap();
    assert!(got.starts_with("Tools: Edit"), "{got}");
}

#[test]
fn test_sample_transcript_small() {
    let messages: Vec<serde_json::Value> = (0..5)
        .map(|i| {
            serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": format!("message {}", i)}
            })
        })
        .collect();
    let result = sample_transcript(&messages, 20, 50_000);
    // All 5 messages should be included (< 2*20)
    assert!(result.contains("message 0"));
    assert!(result.contains("message 4"));
}

#[test]
fn test_sample_transcript_large_samples_ends() {
    let messages: Vec<serde_json::Value> = (0..100)
        .map(|i| {
            serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": format!("msg-{}", i)}
            })
        })
        .collect();
    let result = sample_transcript(&messages, 5, 50_000);
    // First 5 and last 5 should be present
    assert!(result.contains("msg-0"));
    assert!(result.contains("msg-4"));
    assert!(result.contains("msg-95"));
    assert!(result.contains("msg-99"));
    // Middle should be omitted — gap marker appears as [system] since it lacks role/content
    assert!(!result.contains("msg-50"));
    // 11 entries: 5 first + 1 gap + 5 last
    let line_count = result.lines().count();
    assert_eq!(
        line_count, 11,
        "expected 11 lines (5+1+5), got {}",
        line_count
    );
}

#[test]
fn test_sample_transcript_respects_char_cap() {
    let messages: Vec<serde_json::Value> = (0..100)
        .map(|i| {
            serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": format!("message-{}-{}", i, "x".repeat(200))}
            })
        })
        .collect();
    let result = sample_transcript(&messages, 20, 500);
    assert!(result.len() <= 520); // cap + "... (truncated)"
}

#[test]
fn prompts_ask_for_the_shifts_block_by_default() {
    let _guard = crate::story_map::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("CSR_NO_STORY_MAP");

    let ctx = StoryContext {
        current_session_excerpt: "user: hello".to_string(),
        current_v3_enrichment: None,
        related_sessions: vec![],
        project_name: "test".to_string(),
    };
    let first_prompt = build_prompt(&ctx);
    assert!(first_prompt.contains("2-3 sentences"));
    assert!(first_prompt.contains("@shifts"));

    let sum_prompt = summary_story_prompt("summary text", "tail text");
    assert!(sum_prompt.contains("under 800 characters"));
    assert!(sum_prompt.contains("@shifts"));
}

#[test]
fn kill_switch_leaves_both_prompts_byte_identical() {
    let _guard = crate::story_map::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("CSR_NO_STORY_MAP", "1");

    let ctx = StoryContext {
        current_session_excerpt: "user: hello".to_string(),
        current_v3_enrichment: None,
        related_sessions: vec![],
        project_name: "test".to_string(),
    };
    let first_prompt = build_prompt(&ctx);
    let sum_prompt = summary_story_prompt("summary text", "tail text");

    std::env::remove_var("CSR_NO_STORY_MAP");

    assert!(!first_prompt.contains("@shifts"));
    assert!(first_prompt.ends_with("No preamble, no markdown headers."));
    assert!(!sum_prompt.contains("@shifts"));
    assert!(sum_prompt.ends_with("no markdown headers."));
    // the whole prompt is today's, nothing more
    assert_eq!(
        sum_prompt,
        "You are summarizing a coding session for future context restoration after memory compaction.\n\n\
             CLAUDE CODE'S SUMMARY OF THE SESSION (written at its latest compaction, from the whole \
             conversation except its last messages):\nsummary text\n\n\
             THE LAST MESSAGES (the summary did not see these; they show where the work actually \
             stood):\ntail text\n\n\
             Write 3-4 sentences: what the session was about across its whole life and its main \
             threads, the key files/tools used, and where the work stood at the end \
             (done/in-progress/blocked). Where the last messages and the summary differ, trust the last \
             messages. Be specific about technical details. Keep it under 800 characters. No preamble, \
             no markdown headers."
    );
}
