//! The session's task segments and the anchor the recap names: which typed task the session
//! is about, and, since the recap's intent should not be the person's raw words, the first
//! sentence the assistant answered that task with.

use super::COMMIT_WEIGHT;

/// One task: a task prompt, its mid-turn continuations, and the work done
/// until the next task prompt.
#[derive(Default)]
pub(super) struct TaskSegment {
    pub(super) prose: String,
    /// A forwarded side-agent note or dreamer what-if: its own task, never
    /// joined with the prompts around it.
    pub(super) note: bool,
    /// When the segment's last JOINED prompt was sent, if the row had a time.
    pub(super) last_prompt_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) tool_calls: u32,
    pub(super) commits: u32,
    /// The first sentence of the assistant's first text reply under this task
    /// ([`reply_intent`]), once one has been seen.
    pub(super) first_reply: Option<String>,
}

impl TaskSegment {
    fn weight(&self) -> u32 {
        self.tool_calls + COMMIT_WEIGHT * self.commits
    }

    /// Note an assistant text block of the main session under this task: the first one that
    /// renders becomes the task's reply.
    pub(super) fn assistant_text(&mut self, text: &str) {
        if self.first_reply.is_none() {
            self.first_reply = reply_intent(text);
        }
    }
}

/// The assistant's reply as a recap intent: the first sentence of its opening paragraph, with
/// the same cleaning as a typed ask (`recap::clean_intent`), never CSR's own text.
pub(super) fn reply_intent(text: &str) -> Option<String> {
    use crate::import::contamination::has_csr_text;
    let text = crate::extraction::provenance::extractable(&shorten_code_paths(text))?;
    if has_csr_text(&text) {
        return None;
    }
    let intent = crate::hooks::recap::clean_intent(&close_holes(&text))?;
    let first = crate::hooks::recap::first_sentence(&intent).unwrap_or(intent);
    (!has_csr_text(&first)).then_some(first)
}

/// Stands where a path too long to keep stood, between [`shorten_code_paths`]'s two passes.
const DROPPED: char = '\u{0}';

/// An inline-code path the quote stripper would remove (longer than
/// `provenance::INLINE_CODE_KEEP_LEN`, with a '/' and no space) becomes its last component
/// when that is short enough to keep, so the sentence keeps a word where the path stood instead
/// of a hole. When the name is too long too, the clause that held the path goes whole. Fenced
/// blocks, spans the stripper keeps and an unclosed backtick are left as they were.
fn shorten_code_paths(text: &str) -> String {
    let keep = crate::extraction::provenance::INLINE_CODE_KEEP_LEN;
    let mut out = String::with_capacity(text.len());
    for (fence, outside) in text.split("```").enumerate() {
        if fence > 0 {
            out.push_str("```");
        }
        if fence % 2 == 1 {
            out.push_str(outside);
            continue;
        }
        let spans: Vec<&str> = outside.split('`').collect();
        for (index, span) in spans.iter().enumerate() {
            let closed = index + 1 < spans.len();
            if index % 2 == 0 {
                out.push_str(span);
            } else if closed
                && span.len() > keep
                && span.contains('/')
                && !span.contains(char::is_whitespace)
            {
                match span.trim_end_matches('/').rsplit('/').next() {
                    Some(base) if !base.is_empty() && base.len() <= keep => out.push_str(base),
                    _ => out.push(DROPPED),
                }
            } else {
                out.push('`');
                out.push_str(span);
                if closed {
                    out.push('`');
                }
            }
        }
    }
    if out.contains(DROPPED) {
        drop_marked_clauses(&out)
    } else {
        out
    }
}

/// Every clause holding a [`DROPPED`] mark goes. A clause ends at ", ", "; ", ". " or a line
/// end; a dropped clause that closed its sentence hands its end to the kept clause before it
/// ("A, moved it to X." reads "A.").
fn drop_marked_clauses(text: &str) -> String {
    let mut kept: Vec<(&str, String)> = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let (at, len) = [", ", "; ", ". ", "\n"]
            .iter()
            .filter_map(|end| rest.find(end).map(|at| (at, end.len())))
            .min()
            .unwrap_or((rest.len(), 0));
        let (clause, end) = (&rest[..at], &rest[at..at + len]);
        rest = &rest[at + len..];
        if !clause.contains(DROPPED) {
            kept.push((clause, end.to_string()));
            continue;
        }
        let closes = matches!(end, ". " | "\n" | "");
        let open = |end: &str| matches!(end, ", " | "; ");
        if let Some(last) = kept.last_mut().filter(|last| closes && open(&last.1)) {
            let mark = clause
                .trim_end()
                .chars()
                .last()
                .filter(|c| ".!?".contains(*c));
            last.1 = match end {
                ". " => ". ".to_string(),
                _ => mark.map_or(String::new(), String::from) + end,
            };
        }
    }
    kept.into_iter()
        .map(|(clause, end)| clause.to_string() + &end)
        .collect()
}

/// Punctuation a dropped span left with nothing between it: "folder: , which" reads
/// "folder, which", a spaced "( )" goes, and a spaced mark before a space or line end closes
/// up. A word that starts with a mark (".gitignore", "::new") and "foo()" stay as written.
fn close_holes(text: &str) -> String {
    static EMPTY_LEAD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?m)[ \t]*[:;][ \t]+([,.;])([ \t]|$)").unwrap()
    });
    static EMPTY_PARENS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"[ \t]+\([ \t]*\)").unwrap());
    static SPACE_BEFORE_MARK: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?m)[ \t]+([,.;:])([ \t]|$)").unwrap());
    let text = EMPTY_LEAD.replace_all(text, "${1}${2}");
    let text = EMPTY_PARENS.replace_all(&text, "");
    SPACE_BEFORE_MARK
        .replace_all(&text, "${1}${2}")
        .into_owned()
}

/// Fewest words a heaviest-segment fallback intent may have.
const MIN_FALLBACK_WORDS: usize = 6;

/// The anchor task and its segment index. The first task anchors the recap
/// (reviewer labels picked it in 9 of 11 sessions, the most-work segment in 7).
/// If it has no clean rendering, the heaviest other segment with any work is
/// tried; otherwise the intent abstains. Same-session compact recaps instead
/// use the latest renderable typed segment captured by `latest_typed_segment`.
pub(super) fn anchor_segment(segments: &[TaskSegment]) -> (Option<String>, Option<usize>) {
    if segments.is_empty() {
        return (None, None);
    }
    // Heaviest other segment; on equal weight the later one wins.
    let heaviest = (1..segments.len())
        .filter(|&i| segments[i].weight() > 0)
        .max_by(|&a, &b| {
            segments[a]
                .weight()
                .cmp(&segments[b].weight())
                .then(a.cmp(&b))
        });
    // The first task may be cut at a clause when it is over-long.
    if let Some(intent) = crate::hooks::recap::anchor_intent(&segments[0].prose) {
        return (Some(intent), Some(0));
    }
    // A fallback must say enough to stand for the session on its own: a
    // short side request ("stopped check now.") abstains instead.
    if let Some(index) = heaviest {
        if let Some(intent) = crate::hooks::recap::clean_intent(&segments[index].prose)
            .filter(|intent| intent.split_whitespace().count() >= MIN_FALLBACK_WORDS)
        {
            return (Some(intent), Some(index));
        }
    }
    (None, None)
}

/// Compact resumes need the current task, not the session's first one.
/// Skip notes and thin side requests; allow the same clause cut as the first
/// anchor. If no later segment renders, keep `anchor_segment` unchanged.
pub(super) fn latest_typed_segment(segments: &[TaskSegment]) -> Option<(String, usize)> {
    segments
        .iter()
        .enumerate()
        .skip(1)
        .rev()
        .find_map(|(index, task)| {
            if task.note {
                return None;
            }
            crate::hooks::recap::anchor_intent(&task.prose)
                .filter(|intent| intent.split_whitespace().count() >= MIN_FALLBACK_WORDS)
                .map(|intent| (intent, index))
        })
}

#[cfg(test)]
mod tests {
    use super::{close_holes, reply_intent, shorten_code_paths};

    #[test]
    fn a_path_in_a_reply_leaves_its_name_and_never_a_hole() {
        let reply = "The Studio's free space dropped because of one folder: \
                     `/Users/someone/build/swap-eval`, which grew by 265 GB in the \
                     last 3 days.";
        assert_eq!(
            reply_intent(reply).as_deref(),
            Some(
                "The Studio's free space dropped because of one folder: swap-eval, which grew \
                 by 265 GB in the last 3 days."
            )
        );
        // A name too long to keep: its clause goes whole.
        let long = "Moved the cache to `/Volumes/Build/csr-scratch/a-very-long-folder-name-here`, \
                    then rebuilt the index.";
        assert_eq!(
            reply_intent(long).as_deref(),
            Some("then rebuilt the index.")
        );
    }

    #[test]
    fn a_kept_span_and_a_word_that_starts_with_a_mark_stay_as_written() {
        let short = "Pushed `origin/main` and `src/lib.rs`.";
        assert_eq!(shorten_code_paths(short), short);
        let marks = "edited the .gitignore and called foo() via ::new";
        assert_eq!(close_holes(marks), marks);
        assert_eq!(
            close_holes("one folder: , which grew ( ) here ."),
            "one folder, which grew here."
        );
    }

    #[test]
    fn a_dropped_clause_stays_inside_its_own_sentence() {
        let path = "`/Volumes/Build/csr-scratch/a-very-long-folder-name-here`";
        let later = format!("Rebuilt the index. Then moved it to {path}, done.");
        assert_eq!(reply_intent(&later).as_deref(), Some("Rebuilt the index."));
        let last = format!("Checked the disk, moved it to {path}. Then rebuilt.");
        assert_eq!(shorten_code_paths(&last), "Checked the disk. Then rebuilt.");
    }

    #[test]
    fn an_unclosed_backtick_and_a_fenced_path_are_left_to_the_stripper() {
        let open = "Ran `/Volumes/Build/csr-scratch/a-very-long-folder-name-here and stopped";
        assert_eq!(shorten_code_paths(open), open);
        let fenced = "See:\n```\n`/Volumes/Build/csr-scratch/a-very-long-folder-name`\n```\nDone.";
        assert_eq!(shorten_code_paths(fenced), fenced);
    }
}
