//! The `needs_compaction` marker on a generation-served engine.

use super::load_reconciled_gated;
use super::tests::{vg_fixture, VG_N};

/// A generation-served engine's compaction rebuild is a chunk generation; it
/// never rebuilds the reflection graph. A load that marked reflection
/// tombstones there made the daemon's compaction loop build a full chunk
/// generation after every persisted hook start, and the tombstones stayed for
/// the next load to mark again. The index-v3 path still marks them: its
/// rebuild compacts both sides.
#[test]
fn a_generation_served_load_leaves_reflection_tombstones_unmarked() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    storage.delete_reflection("refl-0").unwrap();

    let served = load_reconciled_gated(true, &storage, &index_dir, VG_N, 1, true, None)
        .unwrap()
        .unwrap();
    assert!(served.search.is_generation_served());
    assert!(served.search.reflection_tombstone_count() > 0);
    assert!(
        !crate::search::needs_compaction(&index_dir),
        "a generation-served load asked for a chunk rebuild over reflection tombstones"
    );

    let index_v3 = load_reconciled_gated(false, &storage, &index_dir, VG_N, 1, true, None)
        .unwrap()
        .unwrap();
    assert!(!index_v3.search.is_generation_served());
    assert!(index_v3.search.reflection_tombstone_count() > 0);
    assert!(crate::search::needs_compaction(&index_dir));
}
