//! The hooks the relay declares: one row per `on(...)` line in `register.js`.
//!
//! The rows follow `plugins/csr-companion/hooks/register.tsx` in its own order (so the hook
//! chain Claude Code builds is the one the TypeScript plugin builds), plus one Notify row for
//! `session.measure`. A matcher is JavaScript source, copied verbatim, so that
//! `claude plugin validate` can read it.

/// What the relay does around the engine for one hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `session.start`: run `next`, spawn the engine, tell it.
    Start,
    /// Ask the engine before `next` (it may answer or rewrite), tell it what `next` returned.
    Gate,
    /// Tell the engine without waiting, run `next` at once.
    Notify,
    /// Ask the engine for an element tree; the plain tree is turned into elements here.
    Render,
    /// `turn.step`: pass the stream through, tell the engine what it returned.
    Stream,
    /// A slash command the engine answers.
    Answer,
}

#[derive(Clone, Copy, Debug)]
pub struct Hook {
    /// What the engine's dispatcher is keyed by.
    pub key: &'static str,
    /// The name of the generated function.
    pub name: &'static str,
    pub event: &'static str,
    /// A JavaScript literal, or none.
    pub matcher: Option<&'static str>,
    pub shape: Shape,
    /// `.catch(pass)`: a failure lets the call go on, as the TypeScript plugin's hooks do.
    pub fail_open: bool,
}

const fn hook(
    key: &'static str,
    name: &'static str,
    event: &'static str,
    matcher: Option<&'static str>,
    shape: Shape,
    fail_open: bool,
) -> Hook {
    Hook {
        key,
        name,
        event,
        matcher,
        shape,
        fail_open,
    }
}

pub const HOOKS: &[Hook] = &[
    hook("stop", "stop", "classic.Stop", None, Shape::Notify, true),
    hook(
        "watched",
        "watched",
        "tool.call",
        Some(concat!(
            "{ tool: ['AskUserQuestion', 'Edit', 'Write', 'NotebookEdit', ",
            "'Bash', 'Agent', 'Workflow', 'Monitor'] }"
        )),
        Shape::Gate,
        true,
    ),
    hook(
        "seen",
        "seen",
        "prompt.submit",
        Some("{ origin: { kind: /^(composer|bridge|task-notification)$/ } }"),
        Shape::Gate,
        true,
    ),
    hook(
        "turn_start",
        "turnStart",
        "turn.start",
        None,
        Shape::Notify,
        true,
    ),
    hook(
        "turn_complete",
        "turnComplete",
        "turn.complete",
        None,
        Shape::Notify,
        true,
    ),
    hook("spawned", "spawned", "agent.spawn", None, Shape::Gate, true),
    hook("edit", "edit", "prompt.edit", None, Shape::Notify, true),
    hook("step", "step", "turn.step", None, Shape::Stream, false),
    hook("fork", "fork", "model.fork", None, Shape::Gate, true),
    hook(
        "chapter",
        "chapter",
        "session.compact",
        None,
        Shape::Gate,
        true,
    ),
    hook(
        "guard",
        "guard",
        "tool.call",
        Some("{ tool: 'Bash' }"),
        Shape::Gate,
        true,
    ),
    hook("tool", "tool", "tool.call", None, Shape::Gate, true),
    hook("digit", "digit", "prompt.submit", None, Shape::Gate, true),
    hook(
        "marker",
        "marker",
        "session.compact",
        Some("{ trigger: ['manual', 'auto', 'plugin'] }"),
        Shape::Gate,
        true,
    ),
    hook(
        "band",
        "band",
        "ui.render",
        Some("{ component: 'AbovePrompt' }"),
        Shape::Render,
        true,
    ),
    hook("config", "config", "config.set", None, Shape::Gate, true),
    hook("start", "start", "session.start", None, Shape::Start, false),
    hook(
        "csr",
        "csr",
        "command.run",
        Some("{ command: 'csr' }"),
        Shape::Answer,
        true,
    ),
    hook(
        "carry",
        "carry",
        "session.compact",
        Some("{ trigger: /^(manual|auto|plugin|precompute)$/ }"),
        Shape::Gate,
        true,
    ),
    hook(
        "keep",
        "keep",
        "session.compact",
        Some("{ trigger: /^(manual|auto|plugin)$/ }"),
        Shape::Gate,
        true,
    ),
    hook(
        "pane",
        "pane",
        "ui.render",
        Some("{ component: 'Pane', requestId: 'csr' }"),
        Shape::Render,
        true,
    ),
    hook(
        "measure",
        "measure",
        "session.measure",
        None,
        Shape::Notify,
        true,
    ),
];

/// The hook the dispatcher knows by `key`.
pub fn find(key: &str) -> Option<&'static Hook> {
    HOOKS.iter().find(|h| h.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_names_are_unique_and_valid_identifiers() {
        for (i, h) in HOOKS.iter().enumerate() {
            assert!(h
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_'));
            assert!(!h.name.starts_with(|c: char| c.is_ascii_digit()));
            for other in &HOOKS[i + 1..] {
                assert_ne!(h.key, other.key);
                assert_ne!(h.name, other.name);
            }
        }
    }

    #[test]
    fn one_start_and_one_answer_and_the_register_tsx_count() {
        let of = |s: Shape| HOOKS.iter().filter(|h| h.shape == s).count();
        assert_eq!(of(Shape::Start), 1);
        assert_eq!(of(Shape::Answer), 1);
        assert_eq!(of(Shape::Stream), 1);
        // 21 `on(` rows in register.tsx, plus session.measure.
        assert_eq!(HOOKS.len(), 22);
    }

    #[test]
    fn no_handler_takes_the_name_of_a_relay_helper() {
        const HELPERS: &[&str] = &[
            "post", "act", "settle", "run", "tree", "pushed", "spawn", "gated", "told", "drawn",
            "pass", "register", "ENGINE", "SOCK_DIR", "V", "sock", "gen", "SOON", "LONG", "TRIES",
            "fails", "bytes", "BIG", "armed", "LIMIT", "SLOW", "PAUSE", "stalled",
        ];
        for h in HOOKS {
            assert!(!HELPERS.contains(&h.name), "{}", h.name);
        }
    }

    #[test]
    fn the_start_hook_is_the_one_the_template_names_start() {
        let start = HOOKS.iter().find(|h| h.shape == Shape::Start).unwrap();
        assert_eq!((start.key, start.name), ("start", "start"));
    }
}
