//! The companion's features, behind the relay: one module per feature of the TypeScript plugin
//! (`plugins/csr-companion/hooks/`). Each has a `register(&mut Table)` that adds its entries to the
//! hooks it acts in; `table()` calls them in the one order below.
//!
//! The order is the order the TypeScript's handlers run their statement groups in (the map of
//! them, `hook-map.md`, lists every group by line). Where one feature acts in a hook on both
//! sides of another's group (`digit`: band, then setup_walk, dreamer, then band again), the
//! feature has a `register` and a `register_late`, called in the places the map names.
//!
//! The order of the calls in `table()`:
//!
//! 1. `pane_cmd`: the WP0 `start` (registers `/csr`) and the `csr` answer, so the e2e answers.
//! 2. `usage`: the log row of the session's usage, after `pane_cmd`'s start.
//! 3. `setup_walk`: `start`, `edit`, `digit`, press.
//! 4. `touch`: `tool`.
//! 5. `carry`: `carry`.

pub mod carry;
pub mod pane_cmd;
pub mod setup_walk;
pub mod state;
pub mod touch;
pub mod usage;

use crate::companion::relay::dispatch::Table;

/// Every feature's entries, registered in the fixed order.
pub fn table() -> Table {
    let mut t = Table::new();
    pane_cmd::register(&mut t);
    usage::register(&mut t);
    setup_walk::register(&mut t);
    touch::register(&mut t);
    carry::register(&mut t);
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_registers_each_feature_where_the_map_puts_it() {
        let t = table();
        assert_eq!(t.features("start"), ["pane_cmd", "usage", "setup_walk"]);
        assert_eq!(t.features("csr"), ["pane_cmd"]);
        assert_eq!(t.features("tool"), ["touch"]);
        assert_eq!(t.features("carry"), ["carry"]);
        assert_eq!(t.features("edit"), ["setup_walk"]);
        assert_eq!(t.features("digit"), ["setup_walk"]);
        assert_eq!(t.features("press"), ["setup_walk"]);
        assert!(t.features("band").is_empty());
    }
}
