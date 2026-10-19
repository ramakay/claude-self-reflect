//! The `/csr` command and the pane's own start work: what WP0 and WP1 put in the dispatcher,
//! now the first entries of the `pane_cmd` feature. Later packages add the pane, the statusline
//! and the refresh timer to it.
//!
//! `start` registers the command (register.tsx 2395-2401). `csr` is the one entry for the command,
//! which routes by its first word: `setup` to the walkthrough, anything else to the relay's
//! answer until the dreams text and the pane open land here.

use super::setup_walk;
use crate::companion::relay::dispatch::{handler, Handler, Reply, Table};
use serde_json::json;

fn on_start() -> Handler {
    handler(|cx, _e| async move {
        if cx.respawn {
            // The registration lives in the module, not in the child that died.
            return Ok(Reply::none());
        }
        let command = json!({
            "name": "csr",
            "description": "Open the CSR pane beside Claude; \"/csr setup\" walks you through \
                its settings; \"/csr dreams\" says how the dreamer is pacing itself",
        });
        let _ = cx.host.call("command.register", vec![command]).await;
        Ok(Reply::none())
    })
}

fn on_command() -> Handler {
    handler(|cx, e| async move {
        let args = e["args"].as_str().unwrap_or("");
        if args.split_whitespace().next() == Some("setup") {
            let text = setup_walk::command_setup(&cx).await?;
            return Ok(Reply::answer(json!({ "text": text })));
        }
        let text = format!("csr relay ok pid={} events={}", cx.pid, cx.events);
        Ok(Reply::answer(json!({ "text": text })))
    })
}

pub fn register(t: &mut Table) {
    t.on("start", "pane_cmd", on_start());
    t.on("csr", "pane_cmd", on_command());
}

/// The entries that run after every other feature's `start` (`app::table`).
pub fn register_late(_t: &mut Table) {}
