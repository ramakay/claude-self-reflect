//! The subagent feature's tests: the nine of `tests/companion.test.ts` (2316-2484), 1:1 by name,
//! then the ones the transcript fallback adds. The TypeScript ran a compaction through the whole
//! plugin; here the `keep` entry is run on its own, through the relay's chain, on the scripted
//! host. Nothing reads or writes the real HOME: the fallback reads a temp one.

use super::*;
use crate::companion::{
    app::compactor::rules::KEEP_HEADER,
    relay::{
        dispatch::testing::{fixture_in, run},
        host::fake::World,
    },
};
use std::time::Duration;

fn call(id: &str, tool: &str, input: Value, text: Option<&str>) -> Value {
    let mut call = json!({ "tool_use_id": id, "tool": tool, "input": input });
    if let Some(text) = text {
        call["text"] = json!(text);
    }
    call
}

fn message(role: &str, text: &str, uses: Vec<Value>) -> Value {
    json!({ "role": role, "text": text, "toolUses": uses })
}

const BRIEF_OF_THE_AGENT: &str = "Implement section F2.9 in /repo/plugins/csr-companion.\n\
    Work alone in the worktree and commit once.";

fn subagent() -> Vec<Value> {
    let file = |path: &str| json!({ "file_path": path });
    let edit = |path: &str| json!({ "file_path": path, "old_string": "a", "new_string": "b" });
    vec![
        message("user", BRIEF_OF_THE_AGENT, vec![]),
        message(
            "assistant",
            "Reading the handlers first.",
            vec![call(
                "t1",
                "Read",
                file("/repo/hooks/register.tsx"),
                Some("a file that says all 3 passed"),
            )],
        ),
        message(
            "assistant",
            "",
            vec![call(
                "t2",
                "Edit",
                edit("/repo/hooks/register.tsx"),
                Some("ok"),
            )],
        ),
        message(
            "assistant",
            "",
            vec![
                call(
                    "t3",
                    "Write",
                    json!({ "file_path": "/repo/hooks/subagent.ts", "content": "x" }),
                    Some("ok"),
                ),
                call("t4", "Edit", file("/repo/hooks/register.tsx"), Some("ok")),
            ],
        ),
        message(
            "assistant",
            "Running the plugin tests.",
            vec![call(
                "t5",
                "Bash",
                json!({ "command": "claude plugin test" }),
                Some("running\n  ✓ one\n12 passed, 1 failed\ndone"),
            )],
        ),
        message(
            "assistant",
            "",
            vec![call(
                "t6",
                "Bash",
                json!({ "command": "git commit -m x" }),
                None,
            )],
        ),
        json!({ "role": "user", "text": "", "toolUses": [], "toolResults": [{
            "tool_use_id": "t6", "isError": false,
            "text": "[feat/story-map abc1234] feat(plugin): keep a subagent brief\n 2 files changed, 9 insertions(+)",
        }] }),
        message(
            "assistant",
            "Now writing the tests for the second handler.",
            vec![call(
                "t7",
                "Bash",
                json!({ "command": "git status" }),
                Some("clean"),
            )],
        ),
    ]
}

fn subagent_block_text() -> String {
    [
        "CSR COMPACT KEEP — subagent. Keep these verbatim in the summary; they are the subagent's task and state:",
        "Task brief (its first message, verbatim):",
        BRIEF_OF_THE_AGENT,
        "Commits it made:",
        "- [feat/story-map abc1234] feat(plugin): keep a subagent brief",
        "Files it edited (newest 30):",
        "- /repo/hooks/subagent.ts",
        "- /repo/hooks/register.tsx",
        "Its last test outcome:",
        "- 12 passed, 1 failed",
        "In progress (its newest words):",
        "Now writing the tests for the second handler.",
    ]
    .join("\n")
}

/// The `keep` entry on the scripted host, with a HOME that is `home`.
struct Rig {
    table: Table,
    fx: crate::companion::relay::dispatch::testing::Fixture,
}

fn rig_in(world: World, home: Option<PathBuf>) -> Rig {
    let mut table = Table::new();
    register_with(&mut table, Arc::new(move || home.clone()));
    Rig {
        table,
        fx: fixture_in(world, "sess-1"),
    }
}

fn rig() -> Rig {
    rig_in(World::default(), None)
}

impl Rig {
    async fn keep(&self, e: Value) -> Reply {
        run(&self.table, "keep", &self.fx.cx, e).await
    }

    fn logged(&self) -> Vec<String> {
        self.fx.host.seen.lock().unwrap().logged.clone()
    }
}

fn instructions(reply: &Reply) -> Option<String> {
    let next = reply.next.as_ref()?;
    next["instructions"].as_str().map(str::to_string)
}

fn blocks(messages: Vec<Value>) -> String {
    subagent_block(&messages).unwrap().unwrap()
}

#[tokio::test]
async fn a_compacting_subagent_gets_its_brief_commits_edited_files_last_test_outcome_and() {
    let rig = rig();
    let e = json!({ "trigger": "auto", "instructions": "keep the plan", "agentId": "sub-1",
        "messages": subagent() });
    let told = instructions(&rig.keep(e).await).unwrap();
    let want = format!("keep the plan\n\n{}", subagent_block_text());
    assert_eq!(told, want);
    // the main session's threads are not the subagent's
    assert!(!told.contains("Reminders from CSR"));
    let bytes = format!("{} bytes", subagent_block_text().len());
    let logged = rig.logged();
    assert!(logged
        .iter()
        .any(|line| line.contains("subagent sub-1 compacting (auto)") && line.contains(&bytes)));
    let e = json!({ "trigger": "manual", "agentId": "sub-1", "messages": subagent() });
    // no instructions: the block alone
    assert_eq!(
        instructions(&rig.keep(e).await).unwrap(),
        subagent_block_text()
    );
}

#[tokio::test]
async fn a_subagent_compaction_asks_no_process_no_pane_py_no_model_call() {
    let rig = rig();
    let e = json!({ "trigger": "auto", "agentId": "sub-1", "messages": subagent() });
    let reply = rig.keep(e).await;
    // the one thing it asks of Claude Code is a debug line; no `model.fork`, and the engine runs no
    // process (it has none to run: the entry holds no `Outside`)
    let asked: Vec<String> = rig
        .fx
        .host
        .calls()
        .into_iter()
        .map(|(method, _)| method)
        .collect();
    assert_eq!(asked, ["ui.log"]);
    assert_eq!(instructions(&reply).unwrap(), subagent_block_text());
}

#[tokio::test]
async fn a_subagent_brief_over_6_kb_is_cut_to_its_first_4_kb_and_last_2_kb_with_a_mark() {
    let rig = rig();
    let long = format!(
        "{}{}{}",
        "H".repeat(BRIEF_HEAD),
        "m".repeat(5000),
        "T".repeat(BRIEF_TAIL)
    );
    let e = json!({ "trigger": "auto", "agentId": "sub-1",
        "messages": [{ "role": "user", "text": long, "toolUses": [] }] });
    let told = instructions(&rig.keep(e).await).unwrap();
    let want = format!(
        "{}\n[… 5000 characters cut …]\n{}",
        "H".repeat(BRIEF_HEAD),
        "T".repeat(BRIEF_TAIL)
    );
    assert!(told.contains(&want));
    assert!(!told.contains(&"m".repeat(10)));
    let exact = "e".repeat(6144);
    let e = json!({ "trigger": "auto", "agentId": "sub-1",
        "messages": [{ "role": "user", "text": exact, "toolUses": [] }] });
    let told = instructions(&rig.keep(e).await).unwrap();
    // 6 KB itself is kept whole
    assert!(told.contains(&exact));
    assert!(!told.contains("cut …"));
}

#[tokio::test]
async fn a_transcript_with_nothing_to_keep_leaves_the_instructions_alone() {
    let rig = rig();
    let e = json!({ "trigger": "auto", "instructions": "keep the plan", "agentId": "sub-1",
        "messages": [{ "role": "assistant", "text": "", "toolUses": [] }] });
    let reply = rig.keep(e).await;
    // no rewrite: the compaction goes on with `keep the plan`
    assert!(reply.next.is_none() && reply.answer.is_none());
    assert!(rig.logged().is_empty());
}

#[tokio::test]
async fn a_subagent_compaction_asks_nothing_of_agent_list_a_list_that_never_settles_holds() {
    let mut world = World::default();
    world.hangs.insert("agent.list".into());
    let rig = rig_in(world, None);
    let e = json!({ "trigger": "auto", "instructions": "keep the plan", "agentId": "sub-1",
        "messages": subagent() });
    let done = tokio::time::timeout(Duration::from_millis(1500), rig.keep(e)).await;
    let reply = done.expect("the compaction was held up");
    let want = format!("keep the plan\n\n{}", subagent_block_text());
    assert_eq!(instructions(&reply).unwrap(), want);
    assert!(rig
        .logged()
        .iter()
        .any(|line| line.contains("subagent sub-1 compacting (auto)")));
    assert!(rig
        .fx
        .host
        .calls()
        .iter()
        .all(|(method, _)| method != "agent.list"));
}

#[test]
fn the_subagent_block_is_capped_in_utf_8_bytes_not_characters() {
    let brief = "界".repeat(6144); // 6144 characters, 18 KB
    let block = blocks(vec![
        message("user", &brief, vec![]),
        message("assistant", &"界".repeat(900), vec![]),
    ]);
    assert!(block.len() <= BLOCK_MAX);
    assert!(block.contains("Task brief"));
    // the newest words are capped in bytes too
    let only = blocks(vec![
        message("user", "the brief", vec![]),
        message("assistant", &"界".repeat(900), vec![]),
    ]);
    let mark = "In progress (its newest words):\n";
    let words = &only[only.find(mark).unwrap() + mark.len()..];
    assert!(words.len() <= 600);
    let wide = blocks(vec![message("user", &"界".repeat(2500), vec![])]);
    assert!(regex::Regex::new(r"\[… \d+ characters cut …\]")
        .unwrap()
        .is_match(&wide));
    let mark = "verbatim):\n";
    let kept = &wide[wide.find(mark).unwrap() + mark.len()..];
    assert!(kept.len() <= BRIEF_HEAD + BRIEF_TAIL + 64);
}

#[test]
fn a_cut_never_splits_a_surrogate_pair() {
    // A Rust string holds no lone surrogate, so the TypeScript's `loneSurrogate` check is the
    // `char` boundary here: a cut that split an emoji would not be a `&str` at all.
    // the newest words: 598 characters, then an emoji that straddles the 600-byte cut, then one
    // more character
    let words = format!("{}😀b", "a".repeat(598));
    let block = blocks(vec![
        message("user", "the brief", vec![]),
        message("assistant", &words, vec![]),
    ]);
    assert!(block.contains(&"a".repeat(597)));
    assert!(!block.contains('😀'));
    // the brief's two cut points: the head and the tail each land inside an emoji
    for pad in 0..4 {
        let brief = format!(
            "{}{}{}{}",
            "a".repeat(BRIEF_HEAD - 2 + pad),
            "😀".repeat(400),
            "b".repeat(pad),
            "😀".repeat(300)
        );
        let told = blocks(vec![message("user", &brief, vec![])]);
        assert!(told.contains("characters cut"));
        assert!(told.len() <= BLOCK_MAX);
        // the head and the tail end on whole emoji: no stray byte of one
        let cut = told.find("\n[… ").unwrap();
        assert!(told[..cut].ends_with('a') || told[..cut].ends_with('😀'));
        assert!(told.ends_with('😀'));
    }
}

#[test]
fn a_subagent_block_that_cannot_be_composed_throws_out_of_subagentblock_which_the() {
    // The engine checks the messages it hands a hook, so the handler's own try/catch is out of a
    // test's reach; what it guards is exactly this call.
    let bad = json!({ "role": "user", "text": "the brief", "toolUses": [], "toolResults": [null] });
    assert!(subagent_block(&[bad]).is_err());
    // a text that is not a string throws too
    let bad = json!({ "role": "user", "text": 5, "toolUses": [] });
    assert!(subagent_block(&[bad]).is_err());
}

#[test]
fn the_subagent_block_holds_the_newest_30_files_the_newest_test_run_that_says() {
    let dir = "d".repeat(150);
    let edits: Vec<Value> = (0..40)
        .map(|k| {
            let path = format!("/repo/{dir}/f{k}.rs");
            message(
                "assistant",
                "",
                vec![call(
                    &format!("e{k}"),
                    "Edit",
                    json!({ "file_path": path }),
                    Some("ok"),
                )],
            )
        })
        .collect();
    let runs = vec![
        message(
            "assistant",
            "",
            vec![call(
                "r1",
                "Bash",
                json!({ "command": "cargo test" }),
                Some("test result: ok. 3 passed; 0 failed"),
            )],
        ),
        message(
            "assistant",
            "",
            vec![call(
                "r2",
                "Bash",
                json!({ "command": "ls" }),
                Some("a.rs\nb.rs"),
            )],
        ),
    ];
    let mut messages = vec![message("user", "the brief", vec![])];
    messages.extend(edits);
    messages.extend(runs);
    messages.push(message("assistant", &"z".repeat(2000), vec![]));
    let block = blocks(messages);
    assert!(block.len() <= BLOCK_MAX);
    // the newest result with a test line, not the newest result
    assert!(block.contains("test result: ok. 3 passed; 0 failed"));
    assert!(block.contains("the brief"));
    // the first ten edits fall off the newest 30
    assert!(!block.contains("/f9.rs"));
    // the newest words, 600 bytes with the cut mark
    assert!(block.contains(&"z".repeat(597)));
    assert!(!block.contains(&"z".repeat(598)));
    // nothing to keep: no block
    assert_eq!(
        subagent_block(&[message("assistant", "", vec![])]).unwrap(),
        None
    );
}

// ---------------------------------------------------------------- the transcript fallback

/// A HOME with the subagent's transcript at `.claude/projects/-repo/sess-1/subagents/`.
fn home_with(file: &str, rows: &[Value]) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join(".claude/projects/-repo/sess-1/subagents");
    std::fs::create_dir_all(&folder).unwrap();
    let lines: Vec<String> = rows.iter().map(Value::to_string).collect();
    std::fs::write(folder.join(file), lines.join("\n")).unwrap();
    home
}

fn transcript() -> Vec<Value> {
    vec![
        json!({ "type": "user", "isSidechain": true, "message": { "role": "user",
            "content": BRIEF_OF_THE_AGENT } }),
        json!({ "type": "assistant", "message": { "role": "assistant", "content": [
            { "type": "text", "text": "Editing the handler." },
            { "type": "tool_use", "id": "t1", "name": "Edit",
                "input": { "file_path": "/repo/hooks/subagent.ts" } }] } }),
        json!({ "type": "user", "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "t1", "content": "ok" }] } }),
        json!({ "type": "assistant", "message": { "role": "assistant", "content": [
            { "type": "tool_use", "id": "t2", "name": "Bash", "input": { "command": "git commit" } }] } }),
        json!({ "type": "user", "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "t2", "content": [
                { "type": "text", "text": "[feat/x abc1234] feat: keep it\n 1 file changed" }] }] } }),
        json!({ "type": "summary", "summary": "not a message" }),
        json!({ "type": "assistant", "message": { "role": "assistant", "content": [
            { "type": "text", "text": "Done with the handler." }] } }),
    ]
}

#[tokio::test]
async fn an_omitted_conversation_is_read_from_the_subagent_s_own_transcript() {
    let home = home_with("agent-sub-1.jsonl", &transcript());
    let rig = rig_in(World::default(), Some(home.path().to_path_buf()));
    let e = json!({ "trigger": "auto", "instructions": "keep the plan", "agentId": "sub-1",
        "messagesOmitted": true });
    let reply = rig.keep(e).await;
    let next = reply.next.clone().unwrap();
    let told = instructions(&reply).unwrap();
    assert!(told.starts_with(&format!("keep the plan\n\n{KEEP_HEADER} subagent.")));
    assert!(told.contains(BRIEF_OF_THE_AGENT));
    assert!(told.contains("- [feat/x abc1234] feat: keep it"));
    assert!(told.contains("- /repo/hooks/subagent.ts"));
    assert!(told.contains("Done with the handler."));
    // the flag is spent: the relay puts the conversation back itself
    assert!(next.get("messagesOmitted").is_none());
    assert!(rig
        .logged()
        .iter()
        .any(|line| line.contains("subagent sub-1 compacting (auto)")));
}

#[tokio::test]
async fn an_omitted_conversation_with_no_transcript_file_passes_through_unchanged() {
    let home = home_with("agent-other.jsonl", &transcript());
    let rig = rig_in(World::default(), Some(home.path().to_path_buf()));
    let e = json!({ "trigger": "auto", "instructions": "keep the plan", "agentId": "sub-1",
        "messagesOmitted": true });
    let reply = rig.keep(e).await;
    assert!(reply.next.is_none() && reply.answer.is_none());
    assert!(rig.logged().is_empty());
    // no HOME at all
    let rig = rig_in(World::default(), None);
    let e = json!({ "trigger": "auto", "agentId": "sub-1", "messagesOmitted": true });
    assert!(rig.keep(e).await.next.is_none());
}

#[tokio::test]
async fn an_omitted_conversation_never_reads_outside_the_projects_folder() {
    // an id that walks out of the folder is refused, whatever file it would reach
    let home = home_with("agent-sub-1.jsonl", &transcript());
    std::fs::write(
        home.path().join("agent-x.jsonl"),
        transcript()[0].to_string(),
    )
    .unwrap();
    let rig = rig_in(World::default(), Some(home.path().to_path_buf()));
    for agent in ["../../../../agent-x", "sub-1/../sub-1", "", "a b"] {
        let e = json!({ "trigger": "auto", "agentId": agent, "messagesOmitted": true });
        assert!(rig.keep(e).await.next.is_none(), "{agent:?}");
    }
}

#[tokio::test]
async fn a_small_conversation_is_built_whole_and_a_main_session_compaction_is_left_alone() {
    let rig = rig();
    // a main-session compaction: no agent id, nothing of ours
    let e = json!({ "trigger": "manual", "instructions": "keep the plan", "messages": subagent() });
    assert!(rig.keep(e).await.next.is_none());
    // an event with neither messages nor the omission mark: nothing to build from
    let e = json!({ "trigger": "manual", "agentId": "sub-1" });
    assert!(rig.keep(e).await.next.is_none());
    // a conversation that cannot be read: the instructions it had
    let bad =
        json!([{ "role": "user", "text": "the brief", "toolUses": [], "toolResults": [null] }]);
    let e = json!({ "trigger": "manual", "agentId": "sub-1", "messages": bad });
    assert!(rig.keep(e).await.next.is_none());
    assert!(rig.logged().is_empty());
}
