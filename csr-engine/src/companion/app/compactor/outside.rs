//! What the compactor needs that Claude Code cannot give it through the relay. The TypeScript ran
//! `process.run` (`git`, the removed `pane.py`, `rm`, the engine's private writer), `fs.*` and
//! `env.get`; the engine does these itself, and a test replaces them with a fake.
//!
//! Each call that can wait has its own limit, as the TypeScript's `timeoutMs` had it: a failure or
//! a timeout reads as no answer.

use super::rules::{js_trim_end, native_window_of};
use crate::{
    companion::{
        app::carry::{in_process, Threads},
        compact,
        db::Db,
        dreams, native_warn,
        paths::{self, Paths},
        private,
    },
    process::CommandDeadline,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// How a file's text is cleaned of secrets before it is kept (`engine private write`'s flags).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redact {
    Nothing,
    Json,
}

/// The removed `pane.py` ran within this; one that overran read as no answer.
const PANE_LIMIT: Duration = Duration::from_secs(20);
/// A `git` run for the keep list.
const GIT_LIMIT: Duration = Duration::from_secs(2);

#[async_trait]
// async_trait marks each boxed-future method `#[must_use]`; clippy 1.99 flags that as doubled.
#[allow(clippy::double_must_use)]
pub trait Outside: Send + Sync {
    /// The removed `pane.py --compact-plan --sid SID --ctx CTX`: the plan as JSON; none when it
    /// gave no answer.
    async fn plan(&self, sid: &str, ctx: usize) -> Option<Value>;
    /// The removed `pane.py --repo CWD --keep RECORD`: one row of the dream log.
    async fn keep(&self, cwd: &str, record: Value);
    /// The removed `pane.py --repo CWD --lost --sid SID`: the threads of Lost and found, as JSON.
    async fn lost(&self, cwd: &str, sid: &str) -> Option<Value>;
    /// `git -C CWD ARGS`: what it printed (the end trimmed, a leading space kept) when it exited 0.
    async fn git(&self, cwd: &str, args: &[&str]) -> Option<String>;
    /// `$HOME`, without trailing slashes; none when it is not set.
    async fn home(&self) -> Option<String>;
    /// `engine private write PATH`: the file kept private, 0600.
    async fn write(&self, path: &str, text: &str, redact: Redact) -> Result<()>;
    /// `rm -f PATH`.
    async fn remove(&self, path: &str);
    /// `fs.exists`.
    async fn exists(&self, path: &str) -> bool;
    /// The window Claude Code's own auto-compact works to, when one is set and it is on.
    async fn native_window(&self) -> Option<f64>;
    /// One compaction Claude Code started on its own (`trigger: "auto"`), for the ledger the
    /// native-window warning counts. A failure is nothing.
    async fn native_win(&self, win: Value);
}

/// The engine doing it itself.
pub struct Here {
    threads: Threads,
}

impl Here {
    pub fn new() -> Self {
        Self {
            threads: in_process(),
        }
    }
}

impl Default for Here {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Outside for Here {
    async fn plan(&self, sid: &str, ctx: usize) -> Option<Value> {
        let sid = sid.to_string();
        let work = tokio::task::spawn_blocking(move || {
            serde_json::to_value(compact::compact_plan_json(&Paths::from_env(), &sid, ctx)).ok()
        });
        tokio::time::timeout(PANE_LIMIT, work).await.ok()?.ok()?
    }

    async fn keep(&self, cwd: &str, mut record: Value) {
        let cwd = PathBuf::from(cwd);
        let work = tokio::task::spawn_blocking(move || {
            let paths = Paths::from_env();
            let root = paths::git(&cwd, &["rev-parse", "--show-toplevel"])
                .map(PathBuf::from)
                .or_else(|| Db::open(&paths).ok()?.pane_root(&cwd));
            record["repo"] = serde_json::to_value(root).unwrap_or(Value::Null);
            let _ = dreams::keep(&paths, &record);
        });
        let _ = tokio::time::timeout(PANE_LIMIT, work).await;
    }

    async fn lost(&self, cwd: &str, sid: &str) -> Option<Value> {
        (self.threads)(cwd.to_string(), sid.to_string()).await.ok()
    }

    async fn git(&self, cwd: &str, args: &[&str]) -> Option<String> {
        let cwd = cwd.to_string();
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        tokio::task::spawn_blocking(move || {
            let out = Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(&args)
                .output_with_deadline(GIT_LIMIT)
                .ok()?;
            let text = String::from_utf8_lossy(&out.stdout);
            out.status.success().then(|| js_trim_end(&text).to_string())
        })
        .await
        .ok()?
    }

    async fn home(&self) -> Option<String> {
        let home = std::env::var("HOME").ok()?;
        let home = home.trim_end_matches('/');
        (!home.is_empty()).then(|| home.to_string())
    }

    async fn native_window(&self) -> Option<f64> {
        let settings = self.home().await.map_or(Value::Null, |home| {
            paths::read_json(&Path::new(&home).join(".claude/settings.json"))
        });
        native_window_of(
            &settings,
            std::env::var("CLAUDE_CODE_AUTO_COMPACT_WINDOW")
                .ok()
                .as_deref(),
            std::env::var_os("DISABLE_AUTO_COMPACT").is_some(),
        )
    }

    async fn native_win(&self, win: Value) {
        let Ok(win) = serde_json::from_value::<native_warn::Win>(win) else {
            return;
        };
        let _ = tokio::task::spawn_blocking(move || {
            native_warn::record_win(&Paths::from_env(), &win);
        })
        .await;
    }

    async fn write(&self, path: &str, text: &str, redact: Redact) -> Result<()> {
        let text = match redact {
            Redact::Nothing => text.to_string(),
            Redact::Json => {
                let mut value: Value = serde_json::from_str(text).map_err(|error| {
                    anyhow!("--redact-json was given text that is not JSON: {error}")
                })?;
                private::redact_value(&mut value);
                serde_json::to_string(&value)?
            }
        };
        private::write_atomic(Path::new(path), &text, &private::default_root())?;
        Ok(())
    }

    async fn remove(&self, path: &str) {
        let _ = std::fs::remove_file(path);
    }

    async fn exists(&self, path: &str) -> bool {
        Path::new(path).exists()
    }
}
