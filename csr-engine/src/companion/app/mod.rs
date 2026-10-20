//! The companion's features, behind the relay: one module per feature of the TypeScript plugin
//! (`plugins/csr-companion/hooks/`). Each has a `register(&mut Table)` that adds its entries to the
//! hooks it acts in; `table()` calls them in the one order below.
//!
//! The order is the order the TypeScript's handlers run their statement groups in where that order
//! matters (the map of them, `hook-map.md`, lists every group by line): on `digit` the band holds
//! the digit first and the dreamer runs before the band's note lifecycle; on `step` the fleet, then
//! pace, then the dreamer. A feature that acts in a hook on both sides of another's group has a
//! `register` and a `register_late`.
//!
//! The order of the calls in `table()`:
//!
//! 1. `band`: the held digit on `digit`, the `band` render, its buttons on `press`.
//! 2. `pane_cmd`: the WP0 `start` (registers `/csr`), the `csr` answer, the `pane` render.
//! 3. `usage`: the log row of the session's usage, after `pane_cmd`'s start; `measure`.
//! 4. `touch`, `fleet`, `compactor`, `setup_walk`, `pace`, `story`, `dreamer`, `guard`, `carry`,
//!    `subagent`: each on the keys `TARGET` lists.
//! 5. late: `band` (the note lifecycle on `digit`), `pane_cmd` (the `start` timers, refresh, the
//!    pane opened).

pub mod band;
pub mod carry;
pub mod compactor;
pub mod dreamer;
pub mod fleet;
pub mod guard;
pub mod pace;
pub mod pane_cmd;
pub mod setup_walk;
pub mod state;
pub mod story;
pub mod subagent;
pub mod touch;
pub mod usage;

use crate::companion::relay::dispatch::Table;

/// Every feature's entries, registered in the fixed order.
pub fn table() -> Table {
    let mut t = Table::new();
    band::register(&mut t);
    pane_cmd::register(&mut t);
    usage::register(&mut t);
    touch::register(&mut t);
    fleet::register(&mut t);
    compactor::register(&mut t);
    setup_walk::register(&mut t);
    pace::register(&mut t);
    story::register(&mut t);
    dreamer::register(&mut t);
    guard::register(&mut t);
    carry::register(&mut t);
    subagent::register(&mut t);
    band::register_late(&mut t);
    pane_cmd::register_late(&mut t);
    t
}

/// The finished table: each key's features in the order they run (47 entries).
#[cfg(test)]
const TARGET: &[(&str, &[&str])] = &[
    ("stop", &["compactor"]),
    ("watched", &["fleet", "compactor"]),
    ("seen", &["compactor"]),
    ("turn_start", &["fleet", "compactor"]),
    ("turn_complete", &["fleet", "compactor", "pace", "story"]),
    ("spawned", &["fleet"]),
    ("edit", &["compactor", "setup_walk"]),
    ("step", &["fleet", "pace", "dreamer"]),
    ("fork", &["dreamer"]),
    ("chapter", &["pace", "dreamer"]),
    ("guard", &["guard"]),
    ("tool", &["touch", "fleet"]),
    ("digit", &["band", "fleet", "setup_walk", "dreamer", "band"]),
    ("marker", &["compactor"]),
    ("band", &["band"]),
    ("config", &["compactor"]),
    (
        "start",
        &[
            "pane_cmd",
            "usage",
            "fleet",
            "compactor",
            "setup_walk",
            "pace",
            "dreamer",
            "guard",
            "pane_cmd",
        ],
    ),
    ("csr", &["pane_cmd"]),
    ("carry", &["carry"]),
    ("keep", &["subagent"]),
    ("pane", &["pane_cmd"]),
    ("press", &["band", "pane_cmd", "setup_walk"]),
    ("measure", &["usage"]),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `got` is `want` with some entries left out, the rest in the same order.
    fn in_order(got: &[&str], want: &[&str]) -> bool {
        let mut want = want.iter();
        got.iter().all(|feature| want.any(|w| w == feature))
    }

    #[test]
    fn the_table_registers_each_feature_where_the_map_puts_it() {
        let t = table();
        assert_eq!(TARGET.iter().map(|(_, f)| f.len()).sum::<usize>(), 47);
        for key in t.keys() {
            let want = TARGET.iter().find(|(k, _)| *k == key);
            assert!(want.is_some(), "{key} is not a key the map names");
        }
        // Until every feature is ported, each key is checked for order, not for every entry.
        for (key, want) in TARGET {
            let got = t.features(key);
            assert!(
                in_order(&got, want),
                "{key}: {got:?} is not in the order {want:?}"
            );
        }
        // A finished feature holds every entry the map gives it, each key exactly.
        let finished = [
            "usage",
            "touch",
            "setup_walk",
            "carry",
            "fleet",
            "compactor",
            "pace",
            "story",
            "subagent",
            "guard",
            "pane_cmd",
            "dreamer",
            "band",
        ];
        for (key, want) in TARGET {
            let only = |list: &[&str]| -> Vec<String> {
                list.iter()
                    .filter(|f| finished.contains(f))
                    .map(|f| f.to_string())
                    .collect()
            };
            assert_eq!(only(&t.features(key)), only(want), "{key}");
        }
        // pane_cmd is part-way: its first start entry and its csr answer.
        assert_eq!(t.features("start")[0], "pane_cmd");
        assert_eq!(t.features("csr"), ["pane_cmd"]);
    }

    #[test]
    fn in_order_takes_a_feature_twice_only_where_the_map_has_it_twice() {
        let digit = ["band", "fleet", "setup_walk", "dreamer", "band"];
        assert!(in_order(&["band", "setup_walk", "band"], &digit));
        assert!(!in_order(&["setup_walk", "band", "band"], &digit));
        assert!(!in_order(&["dreamer", "setup_walk"], &digit));
    }
}
