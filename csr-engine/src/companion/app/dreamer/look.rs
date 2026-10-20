//! One look: a fork of the session asking for one "what if...?", the lease
//! it runs under, and what came of it.
//!
//! One dreamer per session: the lease says which copy of the plugin dreams. It is taken before a
//! look and renewed before every write the look makes, so a look whose lease another copy took
//! writes nothing more (money spent is still written down: a fork is paid for).

use super::{
    answers::answered,
    count, int, out,
    text::{
        cited_commits, dream_id, dream_prompt, leans_on_old, mark_receipts, near_dream,
        offers_another_way, parse_dream, parse_sections, seen_entry, Dream,
    },
};
use crate::companion::{
    agents::strip_envelopes,
    app::{
        fleet::{self, agents_text, completion_news, model::parse_agent_envelope, read_ledger},
        pace::{
            allowance, can_afford, change_chapter, change_pace, chapter_of, dreams_left, facts,
            guess, has_news, kept, may_look, pace_of, sides_of, soon, spread, tokens_of,
            usual_steps, Chapter, PaceRow,
        },
        state::{self, strings},
    },
    lease::Step,
    relay::dispatch::Ctx,
    store::MACHINE,
};
use anyhow::Result;
use serde_json::{json, Map, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

/// The last 50 dreams are never asked again.
pub const SEEN_KEPT: usize = 50;
/// How many lines of the brief the dreamer remembers having read.
pub const FACTS_KEPT: usize = 400;
/// The lease is renewed this often while a look runs.
pub const LEASE_RENEW_MS: f64 = 60_000.;

/// What started a look: the main session's own schedule, or the completion of agents (the keys
/// of their identities).
#[derive(Clone, Debug, PartialEq)]
pub enum Trigger {
    Main,
    Agent(Vec<String>),
}

/// What a look did that its end must know: it made a fork, the fork threw.
#[derive(Default)]
pub struct Trace {
    pub forked: bool,
    pub threw: bool,
}

// ---------------------------------------------------------------- the lease

/// A look's hold on the lease. `owns` renews it, one renewal at a time in the order asked (the
/// TypeScript chained them on a promise); a renewal that finds the lease gone ends the timer.
pub struct Tenure {
    cx: Ctx,
    held: AtomicBool,
    queue: tokio::sync::Mutex<()>,
    timer: Mutex<Option<u64>>,
}

impl Tenure {
    fn new(cx: &Ctx) -> Arc<Self> {
        Arc::new(Self {
            cx: cx.clone(),
            held: AtomicBool::new(true),
            queue: tokio::sync::Mutex::new(()),
            timer: Mutex::new(None),
        })
    }

    pub fn held(&self) -> bool {
        self.held.load(Ordering::SeqCst)
    }

    /// Whether this copy still holds the lease, after renewing it.
    pub async fn owns(&self) -> bool {
        let _turn = self.queue.lock().await;
        if self.held() {
            let now = self.cx.clock.now();
            let still = out(&self.cx)
                .lease(&self.cx.sid, Step::Renew, false, now)
                .await;
            self.held.store(still, Ordering::SeqCst);
            if !still {
                self.stop_timer();
            }
        }
        self.held()
    }

    fn stop_timer(&self) {
        let id = self.timer.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(id) = id {
            self.cx.timers.cancel(id);
        }
    }

    /// The look is over: no more renewals, and the last one has finished.
    async fn finish(&self) {
        self.stop_timer();
        let _last = self.queue.lock().await;
    }
}

async fn lease(cx: &Ctx, step: Step, busy: bool) -> bool {
    out(cx).lease(&cx.sid, step, busy, cx.clock.now()).await
}

// ---------------------------------------------------------------- what a look writes

/// What a look spent, as the dream log keeps it beside its tokens: the fork's four counts, and the
/// model that answered. A fork re-sends the main thread's last request, model and all, so that is
/// the model of the step that started the look.
#[derive(Clone, Debug)]
pub struct Spent {
    pub usage: Value,
    pub model: Option<String>,
}

fn spent_fields(spent: Option<&Spent>) -> Map<String, Value> {
    let mut fields = Map::new();
    let Some(spent) = spent else {
        return fields;
    };
    let mut usage = Map::new();
    for name in [
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ] {
        if let Some(count) = spent.usage[name].as_f64() {
            usage.insert(name.to_string(), int(count));
        }
    }
    fields.insert("usage".into(), Value::Object(usage));
    if let Some(model) = spent.model.as_ref().filter(|model| !model.is_empty()) {
        fields.insert("model".into(), json!(model));
    }
    fields
}

/// What every look record carries besides its outcome: the spend check's two sides, how long the
/// look took (brief and fork included), and, when the fork came back without text, why.
fn more(
    cx: &Ctx,
    pace: &PaceRow,
    last_step: f64,
    began: f64,
    reason: &Option<String>,
    trigger: &Trigger,
) -> Map<String, Value> {
    let mut fields = match sides_of(pace, last_step) {
        Value::Object(fields) => fields,
        _ => Map::new(),
    };
    fields.insert("took_ms".into(), int(cx.clock.now() - began));
    fields.insert(
        "budget".into(),
        json!({"spent":pace.dreamer,"cap":super::super::pace::SHARE * pace.claude}),
    );
    if let Some(reason) = reason.as_ref().filter(|reason| !reason.is_empty()) {
        fields.insert("reason".into(), json!(reason));
    }
    if let Trigger::Agent(keys) = trigger {
        fields.insert("trigger".into(), json!("agent"));
        fields.insert("completions".into(), json!(keys.len()));
    }
    fields
}

/// The chapter's change that sets the next look soon, for a look of the main schedule: a look a
/// completion started leaves the chapter's next-look step alone, whatever it came to (it moves
/// only when the look itself used one of the chapter's dreams).
type Change = Box<dyn FnOnce(Chapter) -> Chapter + Send>;

fn soonish(agent: bool, at: f64) -> Change {
    if agent {
        Box::new(|chapter| chapter)
    } else {
        Box::new(soon(at))
    }
}

fn spreadout(agent: bool, at: f64, steps: f64) -> Change {
    if agent {
        Box::new(kept)
    } else {
        Box::new(spread(at, steps))
    }
}

/// What came of a look, kept in the dream log: once you have answered the last dream, the pane
/// shows it in the dream's place, so a quiet dreamer reads as quiet, not as stuck. `every` is the
/// steps from the look to the next.
#[allow(clippy::too_many_arguments)]
async fn look_came_to(
    cx: &Ctx,
    tenure: &Tenure,
    outcome: &str,
    chapter: &Chapter,
    base: f64,
    tokens: f64,
    spent: Option<&Spent>,
    extra: Map<String, Value>,
) -> Result<()> {
    let at = cx.clock.now();
    if !tenure.owns().await {
        return Ok(());
    }
    let mut record = Map::new();
    let mut put = |name: &str, value: Value| record.insert(name.to_string(), value);
    put("kind", json!("look"));
    put("at", int(at / 1000.));
    put("session", json!(cx.sid));
    put("outcome", json!(outcome));
    put("every", int((chapter.due - base).max(0.)));
    put("tokens", int(tokens));
    for (name, value) in spent_fields(spent) {
        put(&name, value);
    }
    put("left", int(dreams_left(chapter)));
    put("of", int(allowance(chapter)));
    for (name, value) in extra {
        put(&name, value);
    }
    (out(cx).keep())(cx.cwd.clone(), Value::Object(record)).await;
    Ok(())
}

// ---------------------------------------------------------------- one look

/// One look, when the chapter allows it. `base` is the chapter step that started it, `model` the
/// model that answered that step.
///
/// `trigger` says what started it: a step of the main session's own schedule (the chapter's), or
/// the completion of agents (their identities, claimed by the caller before this ran). A
/// completion's look is one chance for each of those completions: however it ends, they are
/// settled here (looked, skipped or failed), and none is offered again.
pub async fn dream(
    cx: &Ctx,
    last_step: f64,
    base: f64,
    model: Option<String>,
    trigger: Trigger,
) -> Result<()> {
    let mut trace = Trace::default();
    let done = dream_once(cx, last_step, base, model, &trigger, &mut trace).await;
    if let Trigger::Agent(keys) = &trigger {
        let word = if trace.threw {
            "failed"
        } else if trace.forked {
            "looked"
        } else {
            "skipped"
        };
        let _ = fleet::settled(cx, keys, word).await;
    }
    done
}

async fn dream_once(
    cx: &Ctx,
    last_step: f64,
    base: f64,
    model: Option<String>,
    trigger: &Trigger,
    trace: &mut Trace,
) -> Result<()> {
    // Another copy of this plugin dreams in this session: this one stays quiet, and asks again in
    // a few steps.
    if !lease(cx, Step::Take, true).await {
        if *trigger == Trigger::Main {
            change_chapter(cx, soon(base), None).await?;
        }
        return Ok(());
    }
    let tenure = Tenure::new(cx);
    let renewing = tenure.clone();
    let id = cx.every(LEASE_RENEW_MS, move |_cx| {
        let tenure = renewing.clone();
        async move {
            tenure.owns().await;
        }
    });
    *tenure.timer.lock().unwrap_or_else(|e| e.into_inner()) = Some(id);
    let done = look(cx, &tenure, last_step, base, model, trigger, trace).await;
    // The look has ended, however it ended: the lease stays this copy's, no longer busy.
    tenure.finish().await;
    lease(cx, Step::Release, false).await;
    done
}

fn same(chapter: &Chapter, opening: &Chapter) -> bool {
    chapter.started == opening.started
        && chapter.session == opening.session
        && chapter.gen == opening.gen
}

/// A compaction that started a new chapter while the fork ran: this answer was about the old one,
/// whose context is gone. A change is made only to the chapter the look opened with.
fn ours(opening: Chapter, change: Change) -> Change {
    Box::new(move |current| {
        if same(&current, &opening) {
            change(current)
        } else {
            current
        }
    })
}

#[allow(clippy::too_many_lines)]
async fn look(
    cx: &Ctx,
    tenure: &Arc<Tenure>,
    last_step: f64,
    base: f64,
    model: Option<String>,
    trigger: &Trigger,
    trace: &mut Trace,
) -> Result<()> {
    let agent = matches!(trigger, Trigger::Agent(_));
    let held = {
        let tenure = tenure.clone();
        move || tenure.held()
    };
    let own_chapter = |change: Change| change_chapter(cx, change, Some(&held));
    let cwd = cx.cwd.clone();
    let session = cx.sid.clone();
    let began = cx.clock.now();
    let usual = usual_steps(cx);
    let mut pace = pace_of(cx);
    let mut reason: Option<String> = None;
    let opening = chapter_of(cx).await?;
    if !may_look(&opening) {
        // The chapter's dreams are spent: no look until the next compaction starts a new one.
        let chapter = own_chapter(spreadout(agent, base, usual)).await?;
        let extra = more(cx, &pace, last_step, began, &reason, trigger);
        return look_came_to(cx, tenure, "spent", &chapter, base, 0., None, extra).await;
    }
    let skip = count(cx, "looksToSkip");
    if skip > 0. {
        if !tenure.owns().await {
            return Ok(());
        }
        cx.store
            .set(&cx.sid, "looksToSkip", &json!(int(skip - 1.)))?;
        let chapter = own_chapter(spreadout(agent, base, usual)).await?;
        let extra = more(cx, &pace, last_step, began, &reason, trigger);
        return look_came_to(cx, tenure, "skip", &chapter, base, 0., None, extra).await;
    }
    if is_busy(cx) {
        // A note is up: no question would be seen.
        own_chapter(soonish(agent, base)).await?;
        return Ok(());
    }
    if !can_afford(&pace, last_step) {
        let chapter = own_chapter(soonish(agent, base)).await?;
        let extra = more(cx, &pace, last_step, began, &reason, trigger);
        return look_came_to(cx, tenure, "budget", &chapter, base, 0., None, extra).await;
    }
    let outside = out(cx);
    let full = (outside.brief(&cwd, &session, cx.clock.now()).await).unwrap_or_default();
    // The brief carries the agents as a machine envelope. It is read for who is running and which
    // of the claimed runs reported, and kept out of what the dreamer reads as CSR's memory: the
    // prompt gets it as its own section.
    let envelope = parse_agent_envelope(&full);
    let brief = strip_envelopes(&full);
    let reported = match trigger {
        Trigger::Agent(keys) => {
            let ledger = read_ledger(cx).unwrap_or_default();
            completion_news(envelope.as_ref(), &ledger, keys)
        }
        Trigger::Main => Vec::new(),
    };
    let now = facts(&brief);
    let before = strings(cx, &session, "briefFacts");
    // A chapter's first look is always taken: after a compaction the work has moved on even when
    // CSR's notes have not, and the feed must never wait on them forever. Later looks need a new
    // note. A completion's look gets no such exception: it needs news like any other. The
    // completions it was started for are news in themselves (an agent's report that is not yet
    // in a dream), and nothing else is.
    let first = !agent && opening.looks == 0.;
    let news = has_news(&now, &before) || !reported.is_empty();
    if !first && !news {
        let chapter = own_chapter(soonish(agent, base)).await?;
        let extra = more(cx, &pace, last_step, began, &reason, trigger);
        return look_came_to(cx, tenure, "quiet", &chapter, base, 0., None, extra).await;
    }
    let seen = strings(cx, MACHINE, "seenDreams");
    let known = strings(cx, MACHINE, "knownDreams");
    let asked = strings(cx, &session, "asked");
    if !tenure.owns().await {
        return Ok(());
    }
    // A fork takes no token ceiling (a model.fork request is a prompt and nothing else), so the
    // check above is a guess and one look can cost more. What it did cost is written down the
    // moment the fork returns, before anything asks whether this look is still wanted (a lost
    // lease, a compaction): money spent is spent. Owed past the share, it holds back every look
    // after it. A fork that threw may have been paid for and says nothing of what: it is charged
    // as the guess.
    let prompt = dream_prompt(
        &brief,
        &seen,
        &known,
        &asked,
        &agents_text(envelope.as_ref(), &reported),
    );
    (outside.keep())(
        cwd.clone(),
        json!({"kind":"looking","session":session,
        "at":int(cx.clock.now()/1000.),"outcome":"dreaming",
        "budget":{"spent":pace.dreamer,"cap":super::super::pace::SHARE*pace.claude}}),
    )
    .await;
    if !tenure.owns().await {
        return Ok(());
    }
    trace.forked = true;
    let reply = match cx
        .host
        .call("model.fork", vec![json!({ "prompt": prompt })])
        .await
    {
        Ok(reply) => reply,
        Err(error) => {
            trace.threw = true;
            let charge = |current: PaceRow| PaceRow {
                looks: current.looks + 1.,
                dreamer: current.dreamer + guess(&current, last_step),
                ..current
            };
            change_pace(cx, charge, None).await?;
            return Err(error);
        }
    };
    let has_usage = !reply["usage"].is_null();
    let used = if has_usage {
        tokens_of(&reply["usage"])
    } else {
        0.
    };
    let pay = |current: PaceRow| PaceRow {
        looks: current.looks + 1.,
        dreamer: current.dreamer + used,
        last_look: if used != 0. { used } else { current.last_look },
        ..current
    };
    pace = change_pace(cx, pay, None).await?;
    if !tenure.owns().await {
        return Ok(());
    }
    let answered_text = (reply["isAnswered"] == true).then(|| reply["text"].as_str().unwrap_or(""));
    if answered_text.is_none() {
        reason = reply["reason"].as_str().map(str::to_string);
    }
    let spent = if has_usage {
        let model = match model {
            Some(model) => Some(model),
            None => session_model(cx).await,
        };
        Some(Spent {
            usage: reply["usage"].clone(),
            model,
        })
    } else {
        None
    };
    if !tenure.owns().await {
        return Ok(());
    }
    let mut remembered: Vec<String> = before
        .iter()
        .filter(|line| !now.contains(line))
        .cloned()
        .collect();
    remembered.extend(now.iter().cloned());
    let from = remembered.len().saturating_sub(FACTS_KEPT);
    cx.store
        .set(&session, "briefFacts", &json!(remembered[from..]))?;
    // A compaction that started a new chapter while the fork ran: this answer was about the old
    // one, whose context is gone. Its tokens count; the new chapter is left as the compaction
    // made it.
    if !same(&chapter_of(cx).await?, &opening) {
        let chapter = chapter_of(cx).await?;
        let extra = more(cx, &pace, last_step, began, &reason, trigger);
        return look_came_to(
            cx,
            tenure,
            "stale",
            &chapter,
            base,
            used,
            spent.as_ref(),
            extra,
        )
        .await;
    }
    // No grounding gate: one kept out the only dream acted on. The prompt asks for a receipt; you
    // judge.
    let sections = parse_sections(answered_text.unwrap_or(""));
    let found = answered_text.and_then(parse_dream);
    let repeat = found.as_ref().is_some_and(|found| {
        seen.iter()
            .chain(known.iter())
            .any(|entry| near_dream(&found.line, Some(&found.title), entry))
    });
    // Another way to what your last three prompts decided, or a reason that is only a line older
    // than 30 days: dropped, whatever else it says.
    let decided = match &found {
        Some(found) if !repeat => offers_another_way(found, &asked),
        _ => None,
    };
    let stale = found
        .as_ref()
        .is_some_and(|found| !repeat && decided.is_none() && leans_on_old(found, &brief, &session));
    let Some(found) = found.filter(|_| !repeat && decided.is_none() && !stale) else {
        let change = ours(
            opening.clone(),
            Box::new(move |current| {
                let current = Chapter {
                    looks: current.looks + 1.,
                    ..current
                };
                spreadout(agent, base, usual)(current)
            }),
        );
        let chapter = own_chapter(change).await?;
        let outcome = if repeat {
            "repeat"
        } else if decided.is_some() {
            "decided"
        } else if stale {
            "stale"
        } else {
            "none"
        };
        let mut extra = more(cx, &pace, last_step, began, &reason, trigger);
        for key in ["confusing_claude", "should_be"] {
            extra.insert(key.into(), sections[key].clone());
        }
        if let Some(against) = &decided {
            extra.insert("against".into(), json!(against));
        }
        return look_came_to(
            cx,
            tenure,
            outcome,
            &chapter,
            base,
            used,
            spent.as_ref(),
            extra,
        )
        .await;
    };
    let missing = outside
        .missing_commits(&cwd, &cited_commits(&found.text))
        .await;
    let text = mark_receipts(&found.text, &brief, &session, &missing);
    let at = cx.clock.now();
    let note = note_of(cx, &found, &text, at, used);
    if !tenure.owns().await {
        return Ok(());
    }
    let is_shown = show_owned(cx, &note, "Dream", tenure).await?;
    if !held() {
        return Ok(());
    }
    let more_dreams = |current: PaceRow| PaceRow {
        dreams: current.dreams + 1.,
        ..current
    };
    pace = change_pace(cx, more_dreams, Some(&held)).await?;
    let change = ours(
        opening.clone(),
        Box::new(move |current| {
            let current = Chapter {
                looks: current.looks + 1.,
                shown: current.shown + f64::from(u8::from(is_shown)),
                ..current
            };
            spreadout(agent, base, usual)(current)
        }),
    );
    let chapter = own_chapter(change).await?;
    if !is_shown {
        return Ok(());
    }
    if !tenure.owns().await {
        return Ok(());
    }
    remember_owned(
        cx,
        "seenDreams",
        &seen_entry(&found.line, Some(&found.title)),
        tenure,
    )
    .await?;
    if !held() {
        return Ok(());
    }
    let mut record = Map::new();
    let mut put = |name: &str, value: Value| record.insert(name.to_string(), value);
    put("kind", json!("dream"));
    put("id", note["id"].clone());
    put("at", int(at / 1000.));
    put("session", json!(session));
    put("line", json!(note["line"]));
    put("title", json!(note["title"]));
    put("text", json!(note["text"]));
    put("tokens", int(used));
    for key in ["confusing_claude", "should_be"] {
        put(key, sections[key].clone());
    }
    for (name, value) in spent_fields(spent.as_ref()) {
        put(&name, value);
    }
    put("outcome", json!("dream"));
    put("every", int((chapter.due - base).max(0.)));
    put("left", int(dreams_left(&chapter)));
    put("of", int(allowance(&chapter)));
    for (name, value) in more(cx, &pace, last_step, began, &reason, trigger) {
        put(&name, value);
    }
    // What the dreamer read: with the answer that settles this dream, one labelled example.
    put("brief", json!(brief));
    if !tenure.owns().await {
        return Ok(());
    }
    (outside.keep())(cwd, Value::Object(record)).await;
    // The TypeScript refreshed the pane's data here (`void refresh`); the pane reads its own
    // files and the engine has no copy of that data to refresh.
    Ok(())
}

fn note_of(cx: &Ctx, found: &Dream, text: &str, at: f64, tokens: f64) -> Value {
    json!({
        "id": dream_id(&found.line, at),
        "kind": "dream",
        "file": "",
        "line": found.line,
        "title": found.title,
        "text": text,
        "sketch": [],
        "symbol": null,
        "oid": null,
        "conversation": cx.sid,
        "at": int(at),
        "tokens": int(tokens),
    })
}

async fn session_model(cx: &Ctx) -> Option<String> {
    match cx.host.call("session.model", vec![]).await {
        Ok(Value::String(model)) => Some(model),
        _ => None,
    }
}

/// Whether a note is up: one at a time, so a look would not be seen.
fn is_busy(cx: &Ctx) -> bool {
    let view = cx.store.get(&cx.sid, "view").ok().flatten();
    view.is_some_and(|view| !view.is_null() && view["kind"] != "asked")
}

/// Shows a note unless another is up; true when it was shown. A look that no longer holds the
/// lease shows nothing.
async fn show_owned(cx: &Ctx, note: &Value, tag: &str, tenure: &Tenure) -> Result<bool> {
    let shown_at = cx.clock.now();
    if !tenure.owns().await {
        return Ok(false);
    }
    let mut is_shown = false;
    state::update(cx, "view", |current| {
        // One note at a time.
        if !tenure.held() || current.as_ref().is_some_and(|view| view["kind"] != "asked") {
            return current;
        }
        is_shown = true;
        Some(
            json!({ "kind": "offer", "note": note, "tag": tag, "shownAt": int(shown_at),
            "promptsSurvived": 0 }),
        )
    })?;
    if is_shown && tenure.held() {
        answered(cx, note, "shown", false, Some(tenure)).await?;
    }
    Ok(is_shown)
}

/// Add `value` to the machine's list `key` unless it is there, if the lease is still this copy's.
async fn remember_owned(cx: &Ctx, key: &str, value: &str, tenure: &Tenure) -> Result<()> {
    if strings(cx, MACHINE, key).iter().any(|item| item == value) {
        return Ok(());
    }
    if tenure.owns().await {
        state::remember(cx, key, value, SEEN_KEPT)?;
    }
    Ok(())
}
