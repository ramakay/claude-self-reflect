//! The session's usage. At each start it is written to the relay log once: the measurement WP0
//! made, run after `pane_cmd`'s start and apart from it so a slow answer holds nothing up. On
//! each `measure` the figures are kept for the pane's masthead (`companion::usage`). Both are the
//! engine's own, not features of the TypeScript plugin, so neither has a handler there.

use crate::companion::relay::dispatch::{handler, Reply, Table};
use serde_json::json;

pub fn register(t: &mut Table) {
    t.on("measure", "usage", crate::companion::usage::measure());
    t.on(
        "start",
        "usage",
        handler(|cx, _e| async move {
            if cx.respawn {
                return Ok(Reply::none());
            }
            let log = cx.log.clone();
            cx.host.detach(Box::new(move |host| {
                Box::pin(async move {
                    let usage = host.call("session.usage", vec![]).await.ok();
                    log.row(json!({ "k": "usage", "usage": usage }));
                })
            }));
            Ok(Reply::none())
        }),
    );
}
