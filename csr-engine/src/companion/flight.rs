//! The main page's "In flight" section: the session's agents depth-first, then Codex.
//! pane.py: agents_of, flight_minutes, flight_item, codex_items, flight_section.
use super::{
    agents::{self, Agent, Projection},
    array, codex_runs,
    feed::age_of,
    pyfmt,
    render::text::one_line,
};
use serde_json::{json, Value};

/// Agents drawn before "and N more".
const FLIGHT_SHOWN: usize = 4;
const FLIGHT_HINT: &str = "the agent's words, not yours: click again to fold";

/// '3 rules', '1 rule'; '' for none.
pub fn counted(n: u64, word: &str) -> String {
    if n == 0 {
        return String::new();
    }
    let plural = n == 1 || word.ends_with("ed");
    format!("{n} {word}{}", if plural { "" } else { "s" })
}
/// The last `parts` folders and the file of a path.
pub fn short_path(rel: &str, parts: usize) -> String {
    let all: Vec<&str> = rel.split('/').collect();
    all[all.len().saturating_sub(parts)..].join("/")
}
/// Minutes an agent has worked: from its start (else when it was first seen) to now while it
/// runs, else to its last event.
fn flight_minutes(row: &Agent, now: f64) -> i64 {
    let begun = row.started.filter(|s| *s != 0.).unwrap_or(row.first_seen) / 1000.;
    let end = if agents::is_active(row) {
        now
    } else {
        row.last_at / 1000.
    };
    (((end - begun) / 60.).floor() as i64).max(0)
}
/// One agent as a row: type and model (and "done" once it ended) on the title, which a minute
/// passing never changes; minutes, tools and its last words under it; a flag when it stalled,
/// failed, was killed, said nothing, or wrote a file Claude also wrote. A finished one opens to
/// its report, the agent's words, which is never put in Claude's prompt.
fn flight_item(row: &Agent, depth: usize, unknown: bool, now: f64) -> Value {
    let status = row.status.as_str();
    let done = match status {
        "completed" => "done",
        "failed" => "failed",
        "killed" => "killed",
        _ => "",
    };
    let model = row.model.as_deref().unwrap_or("");
    let base = [
        row.kind
            .as_deref()
            .filter(|k| !k.is_empty())
            .unwrap_or("agent"),
        model,
        done,
    ]
    .into_iter()
    .filter(|x| !x.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let last = if row.last.is_empty() {
        row.last_tool
            .as_deref()
            .filter(|t| !t.is_empty())
            .map(|t| format!("using {t}"))
            .unwrap_or_default()
    } else {
        row.last.clone()
    };
    let body = [
        format!("{}m", flight_minutes(row, now)),
        counted(row.tools, "tool"),
        one_line(&last, 200),
    ]
    .into_iter()
    .filter(|x| !x.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let report = agents::reported(row);
    let more_overlaps = row.overlaps.len() as i64 - 1 + row.overlaps_omitted as i64;
    let overlap = format!(
        "also edited by Claude this session{}{}",
        row.overlaps
            .first()
            .map(|p| format!(": {}", short_path(p, 2)))
            .unwrap_or_default(),
        if more_overlaps > 0 {
            format!(" (+{more_overlaps})")
        } else {
            String::new()
        }
    );
    let flag = if agents::stalled(row, now) {
        format!(
            "stalled {}m",
            ((now - row.last_at / 1000.) / 60.).floor() as i64
        )
    } else if matches!(status, "failed" | "killed") {
        status.to_owned()
    } else if agents::no_report(row) {
        "no report".to_owned()
    } else {
        String::new()
    };
    let flags: Vec<String> = [
        flag,
        if !row.overlaps.is_empty() || row.overlaps_omitted > 0 {
            overlap
        } else {
            String::new()
        },
    ]
    .into_iter()
    .filter(|x| !x.is_empty())
    .collect();
    let mut detail = vec![];
    if !report.is_empty() {
        detail.push("What it found — the agent’s words".to_owned());
        detail.push(one_line(report, 700));
    }
    if !row.edits.is_empty() || row.edits_omitted > 0 {
        let shown = row
            .edits
            .iter()
            .take(4)
            .map(|p| short_path(p, 2))
            .collect::<Vec<_>>()
            .join(", ");
        let rest = row.edits.len() as i64 - 4 + row.edits_omitted as i64;
        detail.push(format!(
            "Wrote: {shown}{}",
            if row.edits.len() > 4 || row.edits_omitted > 0 {
                format!(" (+{rest} more)")
            } else {
                String::new()
            }
        ));
    }
    if unknown {
        detail.push("Its parent is not in this list.".to_owned());
    }
    // a title is wrapped by words, so a tree is drawn in marks, not spaces
    let guide = if depth > 0 {
        format!("{}└ ", "│ ".repeat(depth - 1))
    } else {
        String::new()
    };
    json!({
        "key": format!("flight:{}", row.id),
        "title": format!("{guide}{base}"),
        "lines": 1,
        "body": if body.is_empty() {
            json!([])
        } else {
            json!([format!("{}{body}", " ".repeat(2 * depth))])
        },
        "fit_body": true,
        "flag": flags.first().cloned().unwrap_or_default(),
        "meta": flags.get(1).cloned().unwrap_or_default(),
        "detail": detail,
        "whole_detail": true,
        "send": "",
        "nosend": true,
        "hint": FLIGHT_HINT,
        "unrated": true,
    })
}
/// The Codex part of In flight: the week's line (amber from 75%, red from 90%), then a row per
/// live run. Nothing when there is no ~/.codex, which the snapshot reports as null.
fn codex_items(codex: &Value) -> Vec<Value> {
    if codex.is_null() {
        return vec![];
    }
    let week = &codex["week"];
    let level = week["level"].clone();
    let line = codex_runs::week_line(week, codex["live"].as_u64().unwrap_or(0));
    let mut items = vec![json!({
        "key": "codex:week",
        "title": line,
        "lines": 1,
        "tone": level,
        "quiet": level.is_null(),
        "detail": [],
        "send": "",
        "nosend": true,
        "unrated": true,
    })];
    for row in array(&codex["rows"]) {
        let said = |name: &str, most: usize| {
            row[name]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| one_line(s, most))
        };
        let body = [
            row["minutes"]
                .as_i64()
                .map_or("time unknown".to_owned(), |m| format!("{m}m")),
            row["cwd"]
                .as_str()
                .filter(|c| !c.is_empty())
                .map_or("folder unknown".to_owned(), |c| short_path(c, 2)),
            row["tokens"]
                .as_f64()
                .map_or("tokens unknown".to_owned(), |t| {
                    format!("{} tokens", pyfmt::short(t))
                }),
        ]
        .join(" · ");
        let identity = match (&row["identity"], &row["started"]) {
            (Value::String(s), _) if !s.is_empty() => s.clone(),
            (_, started) => py_float(started.as_f64().unwrap_or(0.)),
        };
        let title = [
            "Codex".to_owned(),
            said("model", 40).unwrap_or("model unknown".into()),
            said("effort", 12).unwrap_or("effort unknown".into()),
        ]
        .join(" · ");
        items.push(json!({
            "key": format!("codex:{identity}"),
            "title": title,
            "lines": 1,
            "body": [body],
            "fit_body": true,
            "detail": [],
            "send": "",
            "nosend": true,
            "unrated": true,
        }));
    }
    items
}
/// A float the way Python prints it: whole numbers keep their ".0".
pub fn py_float(v: f64) -> String {
    format!("{v:?}")
}
/// In flight: the session's agents depth-first, four by default and "and N more" after them,
/// then Codex (its week and its live runs). Open while any agent is pending, running or waiting,
/// or a report came in the last 30 minutes; else one folded row. None without agents and
/// without a ~/.codex.
pub fn flight_section(
    agents_value: &Value,
    codex: &Value,
    now: f64,
    folds: &[Value],
) -> Option<Value> {
    let projection: Projection = serde_json::from_value(agents_value.clone()).unwrap_or_default();
    let rows = &projection.agents;
    let elsewhere = codex_items(codex);
    if rows.is_empty() && elsewhere.is_empty() {
        return None;
    }
    let folded = |name: &str| folds.contains(&json!(name));
    let tree = agents::tree(rows);
    let live = rows.iter().filter(|r| agents::is_active(r)).count() as u64;
    let recent = agents::recent_completion(rows, now);
    let is_open = live > 0 || recent.is_some();
    let times: Vec<f64> = rows.iter().map(|r| r.last_at / 1000.).collect();
    let newest = times
        .iter()
        .copied()
        .fold(None, |m: Option<f64>, t| Some(m.map_or(t, |m| m.max(t))));
    let left = projection.omitted_agents;
    let mut right = if rows.is_empty() {
        String::new()
    } else if is_open {
        [
            if live > 0 {
                format!("{live} running")
            } else {
                "all finished".to_owned()
            },
            if left > 0 {
                format!("+{left} not shown")
            } else {
                String::new()
            },
        ]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
    } else {
        format!("{} finished", rows.len())
    };
    let mut items = vec![];
    let unfolded = folded("flight");
    if !rows.is_empty() && !is_open && !unfolded {
        let ago = age_of(now - newest.unwrap_or(0.));
        items.push(json!({
            "key": "fold:flight",
            "quiet": true,
            "lines": 1,
            "title": format!("› {} finished · {ago} ago", counted(rows.len() as u64, "agent")),
            "detail": [],
            "send": "",
        }));
    } else if !rows.is_empty() {
        if !is_open {
            items.push(json!({
                "key": "fold:flight",
                "quiet": true,
                "lines": 1,
                "title": format!("⌄ {} finished", counted(rows.len() as u64, "agent")),
                "detail": [],
                "send": "",
            }));
        }
        let more = folded("flight-more") || !is_open;
        let shown = if more {
            tree.len()
        } else {
            FLIGHT_SHOWN.min(tree.len())
        };
        items.extend(
            tree[..shown]
                .iter()
                .map(|(row, depth, unknown)| flight_item(row, *depth, *unknown, now)),
        );
        if is_open && tree.len() > FLIGHT_SHOWN {
            let hidden = tree.len() - FLIGHT_SHOWN;
            items.push(json!({
                "key": "fold:flight-more",
                "quiet": true,
                "lines": 1,
                "detail": [],
                "send": "",
                "title": if more { "⌄ fewer".to_owned() } else { format!("› and {hidden} more") },
                "unsigned": true,
            }));
        }
    }
    items.extend(elsewhere);
    let running = codex["live"].as_u64().unwrap_or(0);
    if !codex.is_null() && rows.is_empty() {
        right = if running > 0 {
            format!("{running} live")
        } else {
            String::new()
        };
    }
    let count = if live + running > 0 {
        live + running
    } else {
        rows.len() as u64
    };
    let brief = if live > 0 {
        format!("{live} running")
    } else {
        let agents = counted(rows.len() as u64, "agent");
        if agents.is_empty() {
            format!("{running} live")
        } else {
            agents
        }
    };
    Some(json!({
        "name": "flight",
        "title": "In flight",
        "right": right,
        "items": items,
        "empty": "",
        "count": count,
        "live": true,
        "active": is_open || running > 0,
        "at": newest,
        "brief": brief,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counted_and_short_path() {
        assert_eq!(counted(0, "tool"), "");
        assert_eq!(counted(1, "tool"), "1 tool");
        assert_eq!(counted(3, "tool"), "3 tools");
        assert_eq!(counted(2, "opened"), "2 opened");
        assert_eq!(short_path("a/b/c/d.rs", 2), "c/d.rs");
        assert_eq!(short_path("d.rs", 2), "d.rs");
    }
    #[test]
    fn test_no_agents_and_no_codex_is_no_section() {
        let none = json!({"agents": [], "omitted_agents": 0, "at": null});
        assert!(flight_section(&none, &Value::Null, 0., &[]).is_none());
    }
    #[test]
    fn test_codex_alone_names_its_live_runs_and_week() {
        let none = json!({"agents": [], "omitted_agents": 0, "at": null});
        let codex = json!({
            "rows": [{"identity": null, "started": 100., "model": null, "effort": null,
                "cwd": null, "tokens": null, "minutes": null}],
            "live": 1, "week": null});
        let section = flight_section(&none, &codex, 200., &[]).unwrap();
        assert_eq!(section["right"], "1 live");
        assert_eq!(section["brief"], "1 live");
        let items = section["items"].as_array().unwrap();
        assert_eq!(items[0]["title"], "Codex week unavailable · 1 live");
        assert_eq!(items[0]["quiet"], true);
        assert_eq!(items[1]["key"], "codex:100.0");
        assert_eq!(items[1]["title"], "Codex · model unknown · effort unknown");
        assert_eq!(
            items[1]["body"][0],
            "time unknown · folder unknown · tokens unknown"
        );
    }
}
