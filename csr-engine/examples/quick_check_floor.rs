//! Measure the score separation between fabricated and genuinely-discussed
//! topics, to pick an evidence-based abstention floor for `csr_quick_check`.
//!
//! Read-only against the live DB (rusqlite `SQLITE_OPEN_READ_ONLY`), brute-force
//! exact cosine over every chunk embedding — the exact upper bound of what the
//! HNSW top-1 can return, so the floor derived here is not an artifact of ANN
//! approximation. Run:
//!   cargo run --release --example quick_check_floor
//!
//! Fabricated probes describe events that never happened on this machine.
//! Genuine probes were each confirmed present in `chunks.content` by a
//! substring query before being added here.

use anyhow::Result;
use csr_engine::embeddings::EmbeddingEngine;
use rusqlite::{Connection, OpenFlags};

struct Probe {
    text: &'static str,
    /// true = the topic was never discussed on this machine.
    fabricated: bool,
}

const PROBES: &[Probe] = &[
    // ── Fabricated: none of these ever happened here ──
    Probe {
        text: "deploying csr-engine to Kubernetes with Helm charts",
        fabricated: true,
    },
    Probe {
        text: "migrating csr-engine storage from SQLite to PostgreSQL",
        fabricated: true,
    },
    Probe {
        text: "the team decision to rewrite anukriti in Elixir Phoenix LiveView",
        fabricated: true,
    },
    Probe {
        text: "onboarding call with the new backend contractor about payroll access",
        fabricated: true,
    },
    Probe {
        text: "adding a Redis caching layer in front of the csr-engine search path",
        fabricated: true,
    },
    Probe {
        text: "the GraphQL API gateway we built for anukriti last quarter",
        fabricated: true,
    },
    Probe {
        text: "switching the docs site frontend from React to Svelte",
        fabricated: true,
    },
    Probe {
        text: "buying a new espresso machine for the office kitchen",
        fabricated: true,
    },
    // ── Genuine: each verified present in chunks.content ──
    Probe {
        text: "codegraph witness closure",
        fabricated: false,
    },
    Probe {
        text: "npm OIDC trusted publisher",
        fabricated: false,
    },
    Probe {
        text: "fastembed aarch64",
        fabricated: false,
    },
    Probe {
        text: "HNSW tiny index exact scan fallback",
        fabricated: false,
    },
    Probe {
        text: "integrity check cached in the meta table",
        fabricated: false,
    },
    Probe {
        text: "rmcp 1.6 migration",
        fabricated: false,
    },
    Probe {
        text: "SessionStart memory manifest injection",
        fabricated: false,
    },
    Probe {
        text: "saga reinstatement recall",
        fabricated: false,
    },
    Probe {
        text: "why does fastembed require native aarch64 rust instead of x86_64",
        fabricated: false,
    },
    Probe {
        text: "the codegraph witness closure and binding rate gates",
        fabricated: false,
    },
    Probe {
        text: "publishing the npm package with an OIDC trusted publisher workflow",
        fabricated: false,
    },
    Probe {
        text: "why hooks use catch-all wrappers so they never block Claude Code",
        fabricated: false,
    },
];

fn bytes_to_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na * nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn main() -> Result<()> {
    let home = dirs::home_dir().expect("home dir");
    let db_path = home.join(".claude-self-reflect/csr-engine.db");
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    eprintln!("loading chunk vectors (read-only)...");
    let mut stmt = conn.prepare("SELECT chunk_id, embedding FROM chunk_embeddings")?;
    let vecs: Vec<(String, Vec<f32>)> = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            Ok((id, bytes_to_vec(&bytes)))
        })?
        .filter_map(|r| r.ok())
        .collect();
    eprintln!("{} chunk vectors loaded", vecs.len());

    let engine = EmbeddingEngine::new()?;

    println!("{:<8} {:<7} {:<58} preview", "kind", "top", "probe");
    let mut fab_max = f32::MIN;
    let mut gen_min = f32::MAX;

    for p in PROBES {
        let qv = engine.embed_single(p.text)?;
        let mut best = (f32::MIN, String::new());
        for (id, v) in &vecs {
            let s = cosine(&qv, v);
            if s > best.0 {
                best = (s, id.clone());
            }
        }
        let preview: String = conn
            .query_row("SELECT content FROM chunks WHERE id = ?1", [&best.1], |r| {
                r.get::<_, String>(0)
            })
            .unwrap_or_default()
            .chars()
            .take(70)
            .collect::<String>()
            .replace(['\n', '\r'], " ");

        if p.fabricated {
            fab_max = fab_max.max(best.0);
        } else {
            gen_min = gen_min.min(best.0);
        }
        println!(
            "{:<8} {:<7.3} {:<58} {}",
            if p.fabricated { "FAB" } else { "GENUINE" },
            best.0,
            p.text,
            preview
        );
    }

    println!();
    println!("fabricated max: {:.3}", fab_max);
    println!("genuine min:   {:.3}", gen_min);
    println!("margin:        {:.3}", gen_min - fab_max);
    Ok(())
}
