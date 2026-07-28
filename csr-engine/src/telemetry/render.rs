//! Render telemetry as a pretty terminal report.

pub mod text {
    use crate::telemetry::Telemetry;

    /// Print a pretty text report (no colour — works in any TTY / log capture).
    pub fn print(t: &Telemetry) {
        let bar = "━".repeat(78);
        println!("{}", bar);
        println!("CSR Telemetry Report ({})", t.window);
        println!("{}", bar);
        println!();

        // ── DB / Index ────────────────────────────────────────────────────────
        let db_mb = t.status.db_size_bytes as f64 / 1_048_576.0;
        let health = if t.status.healthy { "ok" } else { "UNHEALTHY" };
        println!(
            "  Index   {:>7} chunks  {:>5} reflections  {:>5} conv  {:>3} proj  db={:.1}MB  health={}",
            t.status.chunks,
            t.status.reflections,
            t.status.conversations,
            t.status.projects,
            db_mb,
            health,
        );
        println!(
            "  Import  {} ({}/{} files)",
            progress_bar(t.status.import_percent),
            t.status.imported_files,
            t.status.total_jsonl_files,
        );
        let e = &t.status.enrichment;
        println!(
            "  Enrich  heuristic={} v3={} ai={}  (v3_failed={} ai_failed={} ai_processing={})",
            e.heuristic_completed,
            e.extracted_v3_completed,
            e.ai_narrative_completed,
            e.extracted_v3_failed,
            e.ai_narrative_failed,
            e.ai_narrative_processing,
        );
        let src = &t.status.aux.sources;
        let miss = &t.status.aux.schema_misses;
        println!(
            "  Sources plans={} docs/{} chunks ({} unscoped)  tasks={} sessions  registry={} sessions",
            src.plan_docs,
            src.plan_chunks,
            src.plan_unscoped_docs,
            src.task_sessions_on_disk,
            src.registry_sessions,
        );
        println!(
            "          proposals={} verdicts={}  schema_miss: tasks={} plans={} history={}",
            src.resolution_proposals, src.resolution_verdicts, miss.tasks, miss.plans, miss.history,
        );
        println!();

        // ── Startup ───────────────────────────────────────────────────────────
        let s = &t.report.startup;
        println!(
            "  Startup  {} total ({} cached, {} rebuilt)",
            s.count, s.cached_count, s.rebuilt_count
        );
        if s.cached_count > 0 {
            println!(
                "           cached:   p50={}ms  p95={}ms",
                s.cached_p50_ms, s.cached_p95_ms
            );
        }
        if s.rebuilt_count > 0 {
            println!(
                "           rebuilt:  p50={}ms  max={}ms     <- cache-miss cost",
                s.rebuilt_p50_ms, s.rebuilt_max_ms
            );
        }
        println!();

        // ── Hook latencies ────────────────────────────────────────────────────
        println!(
            "  Hooks   {} invocations across {} hook types",
            t.report.total_hook_invocations,
            t.report.hooks.len(),
        );
        println!();
        println!(
            "    {:<18}{:>7}{:>9}{:>9}{:>9}{:>9}{:>9}",
            "name", "count", "p50", "p95", "p99", "max", "avg",
        );
        println!("    {}", "─".repeat(70));
        for h in &t.report.hooks {
            println!(
                "    {:<18}{:>7}{:>9}{:>9}{:>9}{:>9}{:>9}",
                truncate(&h.name, 18),
                h.count,
                fmt_ms(h.p50_ms),
                fmt_ms(h.p95_ms),
                fmt_ms(h.p99_ms),
                fmt_ms(h.max_ms),
                fmt_ms(h.avg_ms),
            );
        }
        println!();

        // ── Footer ────────────────────────────────────────────────────────────
        println!(
            "  Source  {}  ({} lines scanned, {} in window)",
            t.log_path, t.log_lines_scanned, t.log_lines_in_window,
        );
        println!("{}", bar);
    }

    fn progress_bar(pct: f64) -> String {
        let filled = ((pct / 10.0).round() as usize).min(10);
        let empty = 10 - filled;
        format!(
            "[{}{}] {:>5.1}%",
            "█".repeat(filled),
            "░".repeat(empty),
            pct
        )
    }

    fn fmt_ms(ms: u64) -> String {
        if ms >= 10_000 {
            format!("{:.1}s", ms as f64 / 1000.0)
        } else if ms >= 1000 {
            format!("{:.2}s", ms as f64 / 1000.0)
        } else {
            format!("{}ms", ms)
        }
    }

    fn truncate(s: &str, max: usize) -> String {
        if s.len() <= max {
            s.to_string()
        } else {
            format!("{}…", &s[..max - 1])
        }
    }
}
