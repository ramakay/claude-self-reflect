//! The session's usage, written to the relay log once at each start: the measurement WP0 made.
//! It is the engine's own, not a feature of the TypeScript plugin, so it has no handler there; it
//! runs after `pane_cmd`'s start and asks apart from the start so a slow answer holds nothing up.

use crate::companion::relay::dispatch::{handler, Reply, Table};
use serde_json::json;

pub fn register(t: &mut Table) {
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
