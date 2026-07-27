//! Live ratatui dashboard for `csr-engine telemetry --tui`.
//!
//! Layout:
//!
//!   ┌────────────────────────────────────────────────────────────────┐
//!   │ header (window, db, health)                                    │
//!   ├──────────────────────────────────┬─────────────────────────────┤
//!   │ Hook latency table               │ Index & Enrichment          │
//!   │ (sorted by p95 desc)             │ (progress bars + counters)  │
//!   ├──────────────────────────────────┴─────────────────────────────┤
//!   │ Startup (cached vs rebuilt) + log scan summary                 │
//!   └────────────────────────────────────────────────────────────────┘
//!
//! Refreshes every 2s. `q` or Esc to quit. `r` to force-refresh.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Table};
use ratatui::{Frame, Terminal};

use super::{collect, Telemetry, Window};

const TICK: Duration = Duration::from_millis(2000);

pub fn run(db_path: PathBuf, projects_dir: PathBuf, window: Window) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let outcome = event_loop(&mut terminal, &db_path, &projects_dir, window);
    restore_terminal(&mut terminal)?;
    outcome
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    db_path: &Path,
    projects_dir: &Path,
    window: Window,
) -> Result<()> {
    let mut telemetry = collect(db_path, projects_dir, window)?;
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| render(f, &telemetry))?;

        let timeout = TICK.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('r') => {
                        telemetry = collect(db_path, projects_dir, window)?;
                        last_tick = Instant::now();
                    }
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= TICK {
            telemetry = collect(db_path, projects_dir, window)?;
            last_tick = Instant::now();
        }
    }
}

fn render(f: &mut Frame, t: &Telemetry) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(10),   // body
            Constraint::Length(7), // footer (startup + log)
        ])
        .split(f.area());

    draw_header(f, outer[0], t);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(outer[1]);

    draw_hooks(f, body[0], t);
    draw_index_panel(f, body[1], t);
    draw_footer(f, outer[2], t);
}

fn draw_header(f: &mut Frame, area: Rect, t: &Telemetry) {
    let health_style = if t.status.healthy {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    };
    let health_label = if t.status.healthy {
        "● healthy"
    } else {
        "● UNHEALTHY"
    };
    let db_mb = t.status.db_size_bytes as f64 / 1_048_576.0;

    let line = Line::from(vec![
        Span::styled(
            "CSR Telemetry",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("   window="),
        Span::styled(&t.window, Style::default().fg(Color::Cyan)),
        Span::raw("   "),
        Span::styled(health_label, health_style),
        Span::raw(format!(
            "   db={:.1}MB  chunks={}  reflections={}  projects={}",
            db_mb, t.status.chunks, t.status.reflections, t.status.projects
        )),
    ]);
    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(" CSR "));
    f.render_widget(p, area);
}

fn draw_hooks(f: &mut Frame, area: Rect, t: &Telemetry) {
    let header = Row::new([
        Cell::from("hook"),
        Cell::from("count"),
        Cell::from("p50"),
        Cell::from("p95"),
        Cell::from("p99"),
        Cell::from("max"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = t
        .report
        .hooks
        .iter()
        .map(|h| {
            let style = severity_style(h.p95_ms);
            Row::new([
                Cell::from(h.name.clone()),
                Cell::from(h.count.to_string()),
                Cell::from(fmt_ms(h.p50_ms)),
                Cell::from(fmt_ms(h.p95_ms)).style(style),
                Cell::from(fmt_ms(h.p99_ms)).style(style),
                Cell::from(fmt_ms(h.max_ms)).style(style),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(20),
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(9),
    ];
    let table = Table::new(rows, widths).header(header).block(
        Block::default().borders(Borders::ALL).title(format!(
            " Hook latencies — {} invocations ",
            t.report.total_hook_invocations
        )),
    );
    f.render_widget(table, area);
}

fn draw_index_panel(f: &mut Frame, area: Rect, t: &Telemetry) {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(5)])
        .split(area);

    // Import progress gauge
    let pct = t.status.import_percent.clamp(0.0, 100.0) as u16;
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(format!(
            " Import  {}/{} files ",
            t.status.imported_files, t.status.total_jsonl_files
        )))
        .gauge_style(Style::default().fg(Color::Green))
        .percent(pct);
    f.render_widget(gauge, parts[0]);

    let e = &t.status.enrichment;
    let mut lines = vec![
        Line::from(vec![Span::styled(
            "Enrichment",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(format!("  heuristic   {}", e.heuristic_completed)),
        Line::from(format!(
            "  v3          {}  (failed {})",
            e.extracted_v3_completed, e.extracted_v3_failed
        )),
        Line::from(format!(
            "  ai          {}  (failed {}, processing {})",
            e.ai_narrative_completed, e.ai_narrative_failed, e.ai_narrative_processing
        )),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Conversations",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(format!(
            "  total       {}  ({} projects)",
            t.status.conversations, t.status.projects
        )),
    ];
    if let Some(ref newest) = t.status.newest_chunk {
        lines.push(Line::from(format!("  newest      {}", newest)));
    }

    let src = &t.status.aux.sources;
    let miss = &t.status.aux.schema_misses;
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "Sources",
        Style::default().add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(format!(
        "  plans       {} docs / {} chunks  ({} unscoped)",
        src.plan_docs, src.plan_chunks, src.plan_unscoped_docs
    )));
    lines.push(Line::from(format!(
        "  tasks       {} sessions on disk",
        src.task_sessions_on_disk
    )));
    lines.push(Line::from(format!(
        "  registry    {} sessions",
        src.registry_sessions
    )));
    lines.push(Line::from(format!(
        "  resolve     {} proposals / {} verdicts",
        src.resolution_proposals, src.resolution_verdicts
    )));
    let total_miss = miss.tasks + miss.plans + miss.history;
    if total_miss > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "  schema_miss tasks={} plans={} history={}",
                miss.tasks, miss.plans, miss.history
            ),
            Style::default().fg(Color::Yellow),
        )));
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Index "));
    f.render_widget(p, parts[1]);
}

fn draw_footer(f: &mut Frame, area: Rect, t: &Telemetry) {
    let s = &t.report.startup;
    let mut lines = vec![Line::from(format!(
        "Startup: {} total  ({} cached, {} rebuilt)",
        s.count, s.cached_count, s.rebuilt_count
    ))];
    if s.cached_count > 0 {
        lines.push(Line::from(format!(
            "  cached   p50={}  p95={}",
            fmt_ms(s.cached_p50_ms),
            fmt_ms(s.cached_p95_ms)
        )));
    }
    if s.rebuilt_count > 0 {
        let style = severity_style(s.rebuilt_max_ms);
        lines.push(Line::from(vec![
            Span::raw("  rebuilt  p50="),
            Span::styled(fmt_ms(s.rebuilt_p50_ms), style),
            Span::raw("  max="),
            Span::styled(fmt_ms(s.rebuilt_max_ms), style),
            Span::raw("    ← cache-miss cost"),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "Source: {}  ({} scanned, {} in window)   [q quit | r refresh]",
        t.log_path, t.log_lines_scanned, t.log_lines_in_window
    )));
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Startup & log "),
    );
    f.render_widget(p, area);
}

fn severity_style(ms: u64) -> Style {
    if ms >= 10_000 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if ms >= 1000 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    }
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
