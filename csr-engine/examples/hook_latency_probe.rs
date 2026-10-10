//! Read-only corpus benchmark. Arguments: binary database index projects cwd prompt [samples].
//! Run with OS write protection on the source data directory: the production
//! renderer can attempt best-effort cache writes. The database is opened read-only.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use csr_engine::{
    embeddings::EmbeddingEngine, engine::Engine, search::SearchEngine, storage::Storage,
};
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(
        args.len() >= 7,
        "binary database index projects cwd prompt [samples]"
    );
    let binary = PathBuf::from(&args[1]);
    let db = PathBuf::from(&args[2]);
    let index = PathBuf::from(&args[3]);
    let projects = PathBuf::from(&args[4]);
    let cwd = PathBuf::from(&args[5]);
    let samples = args
        .get(7)
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(50)
        .max(1);
    let load = std::process::Command::new("/usr/bin/uptime")
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned());
    let temp = tempfile::tempdir()?;
    let storage = Arc::new(Storage::open_read_only(&db)?);
    let chunks = storage.count_chunk_embeddings()?;
    let reflections = storage.count_reflection_embeddings()?;
    let started = Instant::now();
    let search =
        SearchEngine::load_from_disk(&index, chunks, reflections).context("index cache missing")?;
    let load_ms = started.elapsed().as_millis();
    let embeddings = Arc::new(EmbeddingEngine::new()?);
    embeddings.warm()?;
    let engine = Arc::new(Engine::from_parts_with_index_dir(
        storage,
        embeddings,
        Arc::new(tokio::sync::RwLock::new(search)),
        projects.clone(),
        temp.path().join("index"),
    ));
    // The real daemon already runs embedding/search work. Prime one complete
    // pipeline, and report its cost separately instead of conflating initial
    // model inference/page faults with steady warm request latency.
    let primer = csr_engine::hooks::HookInput {
        prompt: Some(args[6].clone()),
        cwd: Some(cwd.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let started = Instant::now();
    let primed = csr_engine::hooks::prompt_submit::render(&primer, &engine, &cwd).await?;
    let first_render_ms = started.elapsed().as_millis();
    let shutdown = Arc::new(AtomicBool::new(false));
    let server = csr_engine::hooks::prompt_transport::spawn_for_daemon(engine, shutdown.clone());
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !csr_engine::hooks::prompt_transport::socket_path(temp.path()).exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .context("socket bind blocked")?;
    let mut times = Vec::new();
    let mut injections = 0;
    let payload = serde_json::to_vec(&serde_json::json!({"prompt": args[6], "cwd": cwd}))?;
    for _ in 0..samples {
        let started = Instant::now();
        let mut child = tokio::process::Command::new(&binary)
            .args(["hook", "prompt-submit", "--db-path"])
            .arg(
                temp.path()
                    .join(db.file_name().context("database filename")?),
            )
            .arg("--projects-dir")
            .arg(&projects)
            .env("CSR_TIMING_LOG", temp.path().join("timing.log"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        child
            .stdin
            .take()
            .context("stdin")?
            .write_all(&payload)
            .await?;
        let output = child.wait_with_output().await?;
        anyhow::ensure!(output.status.success(), "hook failed");
        times.push(started.elapsed().as_millis());
        injections += usize::from(!output.stdout.is_empty());
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    times.sort_unstable();
    println!(
        "{}",
        serde_json::json!({"chunks":chunks,"reflections":reflections,"cache_load_ms":load_ms,
        "first_render_ms":first_render_ms,"primed_bytes":primed.len(),"load":load,
        "samples":samples,"injections":injections,"median_ms":times[samples/2],
        "p95_ms":times[(samples*95).div_ceil(100)-1],"max_ms":times[samples-1]})
    );
    shutdown.store(true, Ordering::SeqCst);
    server.await?;
    Ok(())
}
