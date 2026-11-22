//! The pending call graph the daemon's resolve-drain tests start from.

use super::Storage;

/// A file database with `caller` (b.rs) calling `callee` (a.rs), extracted and
/// not resolved, b.rs pending: what a PostToolUse hook leaves for the drain.
pub(crate) fn seed_pending_graph(storage: &Storage) {
    storage
        .with_connection(|c| {
            for (id, file, name) in [("callee", "a.rs", "callee"), ("caller", "b.rs", "caller")] {
                crate::storage::codegraph::upsert_node(
                    c,
                    &crate::storage::codegraph::NodeRow {
                        id: id.into(),
                        repo: "r".into(),
                        project: "proj".into(),
                        file: file.into(),
                        lang: "rust".into(),
                        kind: "function".into(),
                        name: name.into(),
                        first_conv_id: "c".into(),
                        last_conv_id: "c".into(),
                        ..Default::default()
                    },
                )?;
            }
            crate::storage::codegraph::replace_file_edges(
                c,
                "proj",
                "b.rs",
                &[crate::storage::codegraph::EdgeRow {
                    src_id: "caller".into(),
                    dst_id: "name:callee".into(),
                    kind: "calls".into(),
                    src_file: "b.rs".into(),
                    weight: 1.0,
                    ..Default::default()
                }],
            )?;
            Ok(())
        })
        .unwrap();
    storage
        .upsert_code_file_state_pending("proj", "b.rs", "h")
        .unwrap();
    assert_eq!(storage.pending_resolve_files().unwrap().len(), 1);
}
