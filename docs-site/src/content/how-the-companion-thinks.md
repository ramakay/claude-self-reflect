---
title: How the companion thinks
---

The companion's Dreams setting uses a model while you work. Now and then it asks the model one "what if...?" about your work. This page says what starts a look, what the model sees, what Self-Reflect adds, what is checked, what it costs, and how to stop it. The setting is called Dreams, and it is on by default.

## What starts a look

A look starts from the main session's own steps. A step is one step of Claude's work that Claude Code reports. Self-Reflect counts the main session's steps, not subagents' steps. It counts them only while Dreams is on and a person is at the session.

- **The first look** comes after 6 steps of a new chapter. A chapter starts with the session and ends at a compaction. A chapter also starts over after 2 hours with no compaction.
- **Later looks** come after the number of steps your chapters usually take, less the steps already gone, spread over the dreams left. The wait is never under 6 steps. Until Self-Reflect has seen your chapters, it assumes 120 steps.
- **A finished background agent** can start a look too. Self-Reflect waits for the main session's next step, or 20 seconds of quiet, so agents that finish together share one look.

A chapter holds 5 dreams. A dream you take up in the chapter it came in gives its place back. A chapter allows at most twice as many looks as it has dream places.

A look does not start when:

- someone is not at the session. A headless run never looks;
- a note is already on screen. One note at a time;
- another copy of the plugin already dreams in the session. Only one copy dreams at a time;
- the chapter has used its dreams;
- you typed past the last dreams. The dreamer then skips 1, 2, 4, up to 16 looks;
- the budget says no (see "What it costs");
- Self-Reflect has nothing new. The first look of a chapter is exempt. Any other look needs a new line about your work in hand: code that changed after a past session, a rule you set for a file, or something you built in another session. A look for a finished agent counts the agent's report as new. Lines you typed in past sessions, corrections you typed, and your answers to past dreams do not count as new.

A look that stops because the chapter is spent, because you typed past a dream, because of the budget, or because nothing is new writes a record to the dream log. A look held back by a note on screen, by another copy of the plugin, or by no person at the session writes none.

## What the fork can and cannot see

A look is a fork. Self-Reflect asks Claude Code for a side request on your session. It sends one thing: the prompt. The fork answers once. The prompt tells it there are no follow-up turns.

- **It sees your conversation.** The prompt tells the fork that it shares the conversation but is a separate instance. Self-Reflect records the model of the step that started the look as the one that answered. If the step names none, it records the session's model.
- **It sees the facts Self-Reflect adds.** The next section lists them.
- **It does not stop Claude.** The look runs apart from the main session, which keeps working.
- **It is given no tools.** The request carries the prompt only. It names no tools, and the prompt tells the fork it has none.
- **It does not see your past sessions directly.** It knows about them only through the facts Self-Reflect puts in the prompt.
- **A compaction during a look drops the answer.** It was about a context that is gone. The tokens still count.

The prompt tells the fork to answer in one fixed format, to ask nothing unless a question is worth it, and never to repeat secrets, keys or personal data from the conversation. It asks for the question in the language you write in.

## What Self-Reflect adds

Before each look, Self-Reflect builds a short brief from your machine. It does this in the engine. It does not use a model. The brief goes into the fork's prompt. Each part has a limit.

| Part | Where it comes from | Limit |
|---|---|---|
| Files Claude handled lately | The session's transcript. Files from the last 30 minutes, newest first. If none, the most used files. | 8 files |
| Code that changed after a past session worked on it, or after this session did before its last compaction | Self-Reflect's code-change records, joined to the sessions that worked on that code | 5 lines |
| Rules you set for these files | Corrections you typed about a file in other sessions | 3 lines |
| Things you built in other sessions that came up now | Skills, mods and plugins your other sessions edited, with their descriptions, when the last 60 text entries of this session name them | 3 things |
| Corrections you typed in this repo lately | Your own typed corrections from other sessions, checked against the transcript | 3 lines |
| Lines you typed in past sessions that sit close to this work | Your prompt history. Only lines that sound like ideas ("what if", "someday", "eventually"), 30 to 240 characters | 4 lines |
| Dreams already offered here, and your answers | The dream log, with your own words when you said why | 8 dreams |
| Background agents | What agents are running, and the reports of the ones that just finished | 700 characters a report |

Two more lists go into the prompt: your last 3 prompts, so the fork does not offer back what you just decided, and the dreams already offered or marked "already thought of", 50 of each.

If the session has quiet threads (work a compaction summary stopped listing), the prompt also asks which of them finished. The fork must quote the summary's own words.

How "close to this work" is decided. For the lines you typed in past sessions, Self-Reflect compares stored numbers it already keeps for chunks of conversation. It keeps the 4 closest lines. It does not run a search model during a look. If this session has no stored chunks yet, that part is empty.

What is kept in the brief. The brief holds your own words and your file paths. It goes to the model inside the fork request. The dream log keeps the brief with each dream, so you can see what the dreamer read.

## What is checked before anything is shown

The fork answers with a question and a short explainer. Self-Reflect checks it before it reaches you:

1. **Format.** The reply needs a line that starts with `dream:`. The question must end with a question mark and be 240 characters or fewer. `dream: none` is a valid answer, and the prompt tells the fork to default to it.
2. **Not a repeat.** The question is compared to the dreams already offered and the ones you marked as thought of. The same words, or many of the same key words, drop it.
3. **Not a reversal.** If the dream offers "instead of", "rather than", "in place of" or "replacing" something that one of your last 3 prompts decided, it is dropped.
4. **Not built on an old line.** A line from your history older than 30 days is context only. A dream that rests only on such lines, and cites no commit or newer session, is dropped.
5. **Receipts are marked, never faked.** The fork cites sessions and commits in brackets. A cite of this session reads "this session". A session the brief did not hold is marked "not in CSR's notes: unchecked". A commit the repo does not have is marked "not found in this repo: unchecked". Only the first 5 commit cites are checked. A later cite is left unmarked. So is a cite when git does not answer in time. The dream still shows, with the mark.
6. **Threads need a quote.** A thread moves to "Guessed done" only if its quote is found in a compaction summary newer than the thread. Nothing is deleted.
7. **Counts must add up.** The "Same page?" reading is dropped if its numbers do not add up, or if it counts a miss and lists no turn for it.

Check 5 does not stop a dream. Checks 2 to 4 do. A dream that fails is not shown. The look is still written to the dream log with its reason, and its tokens still count.

The fork also sends optional notes on the session: where Claude misread you, and repeated asks that belong in a skill, memory, hook or CLAUDE.md. Self-Reflect checks their shape and length. It does not check their quotes against the transcript. Treat them as the model's reading.

## What you see, and what reaches Claude

A dream shows in the band above your prompt, and in the herdr pane. One note shows at a time. A note you ignore for 2 prompts goes away and counts as typed past. An open dream is carried over a compaction once, then expires.

Nothing a look finds goes to Claude by itself. A key press does it:

- **Chat in main session** (the band) puts the question and its reasoning in your prompt box, if the box is empty. You press Enter.
- **Sending from the pane** types the text into Claude's prompt. The popup says "Press Enter there to send".

The text is cleaned so an `@`, a paste marker or a word like "ultrathink" reads as plain text, not as a command.

## What it costs

Self-Reflect guesses that a look costs what the last look cost, or the last step if there was no look yet. The request sets no token ceiling, so a look can cost more than the guess.

- **The share.** Before each look, Self-Reflect checks that the dreamer's spend plus the guess stays within 1 token for every 10 Claude has spent in the session. All four counts of a response (input, output, cache read, cache write) are added.
- **A look can run over.** If it does, the excess is owed. Every look waits until Claude's own spending brings the total back within the share.
- **A failed fork is charged as a guess.** A fork that errors may still have been paid for. Self-Reflect charges the guess.
- **The side jobs ride along.** Each look also asks for the session observations, the "Same page?" reading and, when threads wait, the thread check. They are in the same request. They add output tokens to every look, whether or not a dream comes back.
- **Time.** The engine waits up to 80 seconds for the plugin to report back on a request. After that the request fails. The brief is built before the fork. The code sets no time limit on the brief as a whole.

Every look record in the dream log keeps how long the look took. A look whose fork answered with usage also keeps the spend and the model. A look that never forked, such as one held back by the budget, has neither. A fork answer with no usage has neither too.

## How to turn it off

- **`CSR_NO_DREAMING=1`** (or `true`, any case) in the environment Claude Code runs with. It stops the companion's looks: the main schedule, the look for a finished agent, and the carry-over of a dream across a compaction. It wins over a Dreams row that is on. With it set, a dream that is up at a compaction is not carried and not expired. It stays on screen as it was. Set it before you start Claude Code, because the companion's engine starts with Claude Code's environment. Any other value, such as `0`, `false` or an empty value, counts as not set. With it set, `/csr dreams` and the Claude Code pane say "Dreams are stopped. CSR_NO_DREAMING is set where Claude Code runs. Unset it and start Claude Code again to dream." The terminal pane tags its dreamer card "Stopped by CSR_NO_DREAMING" and gives the rest of those words under the tag, wrapped onto rows. A pane with no room left keeps one row of them. If that row ends in "…", a click on it opens the words in full. In a terminal that shows no pictures, its text rows lead with "Dreams are stopped." in place of the tag. The status text ends with "dreams stopped by CSR_NO_DREAMING". If the Dreams row is off too, the words add that dreams are also off in `/config`. No surface offers a press to turn dreams on, since a press cannot help. The terminal pane and the status text learn it from a flag the companion writes to the session's usage file, because herdr starts that pane with its own environment. The flag is written each time Claude Code reports usage. Until a session's first report, those two show the row's state. A flag older than 24 hours is not used. A dream that is already up stays up, and its card has no tag. The terminal pane's picture gives the stopped words under the dream when there is room. Its text rows do not.
- **Dreams in `/config`.** The setting `csr-companion` / Dreams. Off stops the looks and the carry-over, and it takes effect at once, also for the look for a finished agent. It does not cancel a look that is already running, and that look's answer can still show. A dream that is up at a compaction is not carried and not expired. It stays on screen as it was. The row does not stop the daemon's dream cycle.
- **The daemon's dream cycle** also stops on `CSR_NO_DREAMING=1`. The daemon reads the variable from its launchd agent, not from your shell. Setup fills that agent from the environment that setup runs in (`carried_process_env` in setup.rs copies its `CSR_*` variables). So an installed daemon sees a change only after you run `csr-engine setup` again with the variable set. Setup keeps the keys the agent already has, and a value that this run gives wins over the one in the agent. To turn the daemon's dreaming on again for an agent that setup manages, run `CSR_NO_DREAMING=0 csr-engine setup`. Setup unloads the job, writes the new file and loads the job again. For a plist you wrote yourself, edit the file, then run `launchctl bootout` and `launchctl bootstrap` on it. Setup does not rewrite that file, and `launchctl kickstart` does not read it again.
- **`CSR_NO_AI_NARRATIVES=1` does not stop the looks.** It covers the briefing, stories, ratification and dream threads. See [Privacy & Security](#/docs/privacy).
- **`CSR_NO_YSK_BRIEF=1`** stops Self-Reflect from adding facts to You should know's requests. It does not stop looks. Neither the Dreams row nor `CSR_NO_DREAMING=1` stops those facts.

## You should know, and other plugins

You should know is a separate plugin that also forks your session. When it forks, Self-Reflect can add its own facts to that fork's request: code a later commit changed, rules you set, corrections you typed, and lines you typed in past sessions. It adds them only to You should know's propose request. The facts are built at most once every 5 minutes for a session. Every propose request in that time gets them. Another plugin's fork is noted in the dream log and passes untouched. `CSR_NO_YSK_BRIEF=1` turns the addition off.

## Where this lives in the source

- `csr-engine/src/companion/app/dreamer.rs`: entries, You should know gate, compaction carry
- `csr-engine/src/companion/app/dreamer/look.rs`: one look, its gates and records
- `csr-engine/src/companion/app/dreamer/prompt.rs` and `text.rs`: the prompt, the reply parser and the checks
- `csr-engine/src/companion/app/dreamer/answers.rs`: answers, back-off, what you send to Claude
- `csr-engine/src/companion/app/pace/mod.rs`: chapters, steps, the share
- `csr-engine/src/companion/app/state.rs`: the `CSR_NO_DREAMING` switch
- `csr-engine/src/companion/app/fleet.rs`: background agents and their reports
- `csr-engine/src/companion/brief.rs`, `note.rs`, `recall.rs`, `dreams.rs`: the brief
- `csr-engine/src/companion/relay/` (`host.rs`, `dispatch.rs`, `register.js.j2`): how a fork travels to Claude Code and back
- `csr-engine/src/companion/retire.rs`, `same_page.rs`: the thread and reading checks
