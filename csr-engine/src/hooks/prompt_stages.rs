//! Step times of one daemon prompt answer, and the one timing-log line a slow
//! answer leaves.
//!
//! The render's steps are timed where they happen (embedding, the wait for
//! the vector index lock, the index search, composing the block) and summed
//! into a task-local record that exists only inside [`measure`]. The same code
//! run anywhere else, the hook's in-process fallback included, records nothing.
//!
//! The line carries numbers, the outcome and the prompt id the hook's own row
//! already carries. Never prompt text, never a path.

use std::cell::RefCell;
use std::time::{Duration, Instant};

/// An answer that takes at least this long, from accept to the end of the
/// reply, leaves one line of step times. So does an answer that ends after
/// its deadline, however fast.
pub(crate) const SLOW_ANSWER: Duration = Duration::from_millis(200);

/// A render step timed where it happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// Embedding text with the local model: the prompt, the anti-pattern query.
    Embed,
    /// Waiting for the read lock on the vector index.
    IndexWait,
    /// Searching the vector index under that lock.
    Search,
    /// Building the block from the ranked results, the send gate included.
    Compose,
}

/// The timed steps of one render.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RenderSteps {
    pub(crate) embed: Duration,
    pub(crate) index_wait: Duration,
    pub(crate) search: Duration,
    pub(crate) compose: Duration,
}

impl RenderSteps {
    fn add(&mut self, stage: Stage, elapsed: Duration) {
        let slot = match stage {
            Stage::Embed => &mut self.embed,
            Stage::IndexWait => &mut self.index_wait,
            Stage::Search => &mut self.search,
            Stage::Compose => &mut self.compose,
        };
        *slot += elapsed;
    }

    fn timed(&self) -> Duration {
        self.embed + self.index_wait + self.search + self.compose
    }
}

tokio::task_local! {
    static STEPS: RefCell<RenderSteps>;
}

/// Run `render` with step timing on. Returns its output and its step times.
pub(crate) async fn measure<F: std::future::Future>(render: F) -> (F::Output, RenderSteps) {
    STEPS
        .scope(RefCell::new(RenderSteps::default()), async {
            let output = render.await;
            let steps = STEPS.with(|steps| *steps.borrow());
            (output, steps)
        })
        .await
}

/// Adds the time from [`time`] until it drops to its step, inside [`measure`]
/// only.
pub(crate) struct StageTimer {
    stage: Stage,
    started: Instant,
}

/// Start timing `stage`; the time counts when the returned timer drops.
pub(crate) fn time(stage: Stage) -> StageTimer {
    StageTimer {
        stage,
        started: Instant::now(),
    }
}

impl Drop for StageTimer {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        let _ = STEPS.try_with(|steps| steps.borrow_mut().add(self.stage, elapsed));
    }
}

/// How a daemon prompt answer ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Written and acknowledged by the hook.
    Delivered,
    /// Rendered in time, but the write or the acknowledgement did not land.
    Undelivered,
    /// Past the deadline before the render started or when it ended; dropped.
    Late,
    /// The render slot did not come free before the deadline.
    SlotTimeout,
    /// Maintenance held the database; nothing was rendered.
    Busy,
    /// The render or its reply could not be built.
    Failed,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Undelivered => "undelivered",
            Self::Late => "late",
            Self::SlotTimeout => "slot_timeout",
            Self::Busy => "busy",
            Self::Failed => "failed",
        }
    }
}

/// The times one answer's line carries, each from the moment the daemon
/// accepted the connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AnswerTimes {
    /// Accept to the end of the answer.
    pub(crate) total: Duration,
    /// Waiting for the one-at-a-time render slot.
    pub(crate) slot_wait: Duration,
    /// The whole render; `steps` are its timed parts.
    pub(crate) render: Duration,
    pub(crate) steps: RenderSteps,
    /// Writing the reply and waiting for the hook's acknowledgement.
    pub(crate) reply: Duration,
    /// When the deadline fell.
    pub(crate) deadline: Duration,
}

/// The line, numbers only: `db` is the render's time outside its timed steps
/// (SQLite reads and the filtering between them); `other` is the answer's
/// time outside the slot wait, the render and the reply (reading the request,
/// the hand-off to a worker).
pub(crate) fn slow_answer_line(
    outcome: Outcome,
    times: &AnswerTimes,
    connected: bool,
    prompt_id: Option<&str>,
) -> String {
    let ms = |d: Duration| d.as_millis();
    let steps = &times.steps;
    let db = times.render.saturating_sub(steps.timed());
    let other = times
        .total
        .saturating_sub(times.slot_wait + times.render + times.reply);
    format!(
        "CSR prompt-answer slow: outcome={} total={}ms slot_wait={}ms embed={}ms index_wait={}ms search={}ms db={}ms compose={}ms reply={}ms other={}ms deadline={}ms connected={} prompt_id={}",
        outcome.label(),
        ms(times.total),
        ms(times.slot_wait),
        ms(steps.embed),
        ms(steps.index_wait),
        ms(steps.search),
        ms(db),
        ms(steps.compose),
        ms(times.reply),
        ms(other),
        ms(times.deadline),
        u8::from(connected),
        plain_id(prompt_id),
    )
}

/// The prompt id as the hook's row writes it, or `-` when it is missing or is
/// not a plain id.
fn plain_id(prompt_id: Option<&str>) -> &str {
    prompt_id
        .map(str::trim)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 64
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .unwrap_or("-")
}

/// Whether an answer earns a line.
pub(crate) fn is_slow(total: Duration, after_deadline: bool) -> bool {
    after_deadline || total >= SLOW_ANSWER
}

/// Write the line to the timing log when the answer [`is_slow`].
pub(crate) fn log_if_slow(
    outcome: Outcome,
    times: &AnswerTimes,
    after_deadline: bool,
    connected: bool,
    prompt_id: Option<&str>,
) {
    if is_slow(times.total, after_deadline) {
        crate::telemetry::append_timing_line(&slow_answer_line(
            outcome, times, connected, prompt_id,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_slow_answer_line_is_pinned_and_carries_no_prompt_row_marker() {
        let times = AnswerTimes {
            total: ms(486),
            slot_wait: ms(157),
            render: ms(300),
            steps: RenderSteps {
                embed: ms(40),
                index_wait: ms(2),
                search: ms(30),
                compose: ms(8),
            },
            reply: ms(1),
            deadline: ms(450),
        };
        let line = slow_answer_line(
            Outcome::Late,
            &times,
            false,
            Some("fd3637d6-0d6b-4c43-9f53-1f0d2b8e4a10"),
        );
        assert_eq!(
            line,
            "CSR prompt-answer slow: outcome=late total=486ms slot_wait=157ms embed=40ms \
             index_wait=2ms search=30ms db=220ms compose=8ms reply=1ms other=28ms \
             deadline=450ms connected=0 prompt_id=fd3637d6-0d6b-4c43-9f53-1f0d2b8e4a10"
        );
        // The review recount and the product's own delivery count read prompt
        // rows by `hook prompt-submit [`; recap rows by `session-start`; the
        // privacy scan flags quoted prompt text.
        for marker in [
            "hook prompt-submit [",
            "session-start",
            "query=\"",
            "prompt=\"",
            "preview=\"",
        ] {
            assert!(!line.contains(marker), "{marker}");
        }
        for (id, shown) in [
            (None, "-"),
            (Some(""), "-"),
            (Some("two words"), "-"),
            (Some("a\nline"), "-"),
            (Some("ok_id-1"), "ok_id-1"),
        ] {
            let line = slow_answer_line(Outcome::Delivered, &times, true, id);
            assert!(
                line.ends_with(&format!(" connected=1 prompt_id={shown}")),
                "{line}"
            );
        }
        assert!(!is_slow(ms(199), false));
        assert!(is_slow(SLOW_ANSWER, false));
        assert!(is_slow(ms(5), true));
    }
}
