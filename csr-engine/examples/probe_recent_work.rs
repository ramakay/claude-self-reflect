//! Receipts probe for inbox-B1 follow-up (queries.rs get_recent_work fixes).
//! Opens the live DB **read-only** and times get_recent_chunks for the
//! unscoped and scoped branches. Never writes.

use csr_engine::storage::Storage;
use std::path::Path;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let db_path = std::env::var("CSR_DB_PATH").unwrap_or_else(|_| {
        "/Users/ramakrishnanannaswamy/.claude-self-reflect/csr-engine.db".into()
    });
    let storage = Storage::open_read_only(Path::new(&db_path))?;

    for (label, project) in [
        ("unscoped (project=None)", None),
        (
            "scoped (project=claude-self-reflect)",
            Some("claude-self-reflect"),
        ),
    ] {
        let mut times = Vec::new();
        for i in 0..3 {
            let start = Instant::now();
            let chunks = storage.get_recent_chunks(6, project)?;
            let elapsed = start.elapsed();
            times.push(elapsed);
            if i == 0 {
                println!("--- {label} ---");
                for c in &chunks {
                    let preview: String = c.content.chars().take(60).collect();
                    println!(
                        "  {} [{}] {} | {:?}",
                        c.conversation_id, c.timestamp, c.is_sidechain, preview
                    );
                }
            }
        }
        println!(
            "{label}: run1={:?} run2(warm)={:?} run3(warm)={:?}",
            times[0], times[1], times[2]
        );
    }

    Ok(())
}
