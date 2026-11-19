//! Progress lines for a long import that runs in front of a person (`csr-engine setup`).
//!
//! The importer reports `ImportProgress` after every file and after every embed batch inside a
//! file. The printer decides what is worth a line: the file count up front, then at most one line
//! every [`PROGRESS_EVERY`], then the last state once. The clock is an argument (elapsed time
//! since the import began), so the cadence is a pure function of its inputs and tests use a fake
//! clock. The daemon never builds a printer: it calls the importer without a callback.

use std::io::{IsTerminal, Write};
use std::time::Duration;

/// The least time between two progress lines.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(3);

/// Where an import stands: files finished (imported, vanished or failed) of the files found, and
/// the chunks written so far, including those of the file in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportProgress {
    pub done: usize,
    pub total: usize,
    pub chunks: usize,
}

/// `12345` as `12,345`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `  Codex: 12 of 34 rollouts, 6,410 chunks`.
pub fn progress_line(label: &str, noun: &str, p: &ImportProgress) -> String {
    format!(
        "  {label}: {} of {} {noun}, {} chunks",
        thousands(p.done),
        thousands(p.total),
        thousands(p.chunks)
    )
}

/// `  Codex: 34 rollouts to import`, printed once before the first file.
pub fn upfront_line(label: &str, noun: &str, total: usize) -> String {
    format!("  {label}: {} {noun} to import", thousands(total))
}

/// Writes progress lines. On a terminal one line is redrawn in place; otherwise each line is
/// plain text ending in a newline, so a log or a pipe stays readable.
pub struct ProgressPrinter<W: Write> {
    out: W,
    tty: bool,
    label: &'static str,
    noun: &'static str,
    started: bool,
    last_printed: Option<Duration>,
    final_printed: bool,
    line_open: bool,
}

impl ProgressPrinter<std::io::Stderr> {
    /// A printer on stderr, redrawing in place when stderr is a terminal.
    pub fn stderr(label: &'static str, noun: &'static str) -> Self {
        let stderr = std::io::stderr();
        let tty = stderr.is_terminal();
        Self::new(stderr, tty, label, noun)
    }
}

impl<W: Write> ProgressPrinter<W> {
    pub fn new(out: W, tty: bool, label: &'static str, noun: &'static str) -> Self {
        Self {
            out,
            tty,
            label,
            noun,
            started: false,
            last_printed: None,
            final_printed: false,
            line_open: false,
        }
    }

    /// Take one report. `elapsed` is the time since the import began. Writing is best effort: a
    /// closed stderr must never fail an import.
    pub fn report(&mut self, elapsed: Duration, p: ImportProgress) {
        if p.total == 0 {
            return;
        }
        if !self.started {
            self.started = true;
            self.last_printed = Some(elapsed);
            let line = upfront_line(self.label, self.noun, p.total);
            let _ = writeln!(self.out, "{line}");
        }
        let finished = p.done >= p.total;
        if finished && self.final_printed {
            return;
        }
        let due = self
            .last_printed
            .is_none_or(|last| elapsed.saturating_sub(last) >= PROGRESS_EVERY);
        if !finished && !due {
            return;
        }
        self.last_printed = Some(elapsed);
        self.final_printed = finished;
        let line = progress_line(self.label, self.noun, &p);
        if self.tty {
            let _ = write!(self.out, "\r\x1b[2K{line}");
            self.line_open = true;
            if finished {
                self.finish();
            }
        } else {
            let _ = writeln!(self.out, "{line}");
        }
        let _ = self.out.flush();
    }

    /// End the redrawn line, so the next thing printed starts on a fresh one.
    pub fn finish(&mut self) {
        if self.line_open {
            let _ = writeln!(self.out);
            self.line_open = false;
        }
        let _ = self.out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: f64) -> Duration {
        Duration::from_secs_f64(secs)
    }

    fn p(done: usize, total: usize, chunks: usize) -> ImportProgress {
        ImportProgress {
            done,
            total,
            chunks,
        }
    }

    fn plain() -> ProgressPrinter<Vec<u8>> {
        ProgressPrinter::new(Vec::new(), false, "Codex", "rollouts")
    }

    fn text(printer: &ProgressPrinter<Vec<u8>>) -> String {
        String::from_utf8(printer.out.clone()).unwrap()
    }

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(6_410), "6,410");
        assert_eq!(thousands(18_103), "18,103");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn the_line_matches_the_documented_shape() {
        assert_eq!(
            progress_line("Codex", "rollouts", &p(12, 34, 6_410)),
            "  Codex: 12 of 34 rollouts, 6,410 chunks"
        );
        assert_eq!(
            upfront_line("Codex", "rollouts", 34),
            "  Codex: 34 rollouts to import"
        );
    }

    #[test]
    fn the_file_count_comes_first_then_one_line_per_interval() {
        let mut printer = plain();
        printer.report(at(0.0), p(0, 34, 0));
        printer.report(at(1.0), p(1, 34, 500)); // too soon
        printer.report(at(2.9), p(2, 34, 900)); // still too soon
        printer.report(at(3.0), p(3, 34, 1_400)); // 3 s since the up-front line
        printer.report(at(4.0), p(4, 34, 1_900)); // too soon
        printer.report(at(6.2), p(5, 34, 2_400)); // 3.2 s since the last line
        assert_eq!(
            text(&printer),
            "  Codex: 34 rollouts to import\n\
             \x20 Codex: 3 of 34 rollouts, 1,400 chunks\n\
             \x20 Codex: 5 of 34 rollouts, 2,400 chunks\n"
        );
    }

    #[test]
    fn the_last_state_always_prints_once_even_inside_the_interval() {
        let mut printer = plain();
        printer.report(at(0.0), p(0, 2, 0));
        printer.report(at(0.5), p(1, 2, 10));
        printer.report(at(0.6), p(2, 2, 20));
        printer.report(at(0.7), p(2, 2, 20)); // a repeat of the end is not printed again
        assert_eq!(
            text(&printer),
            "  Codex: 2 rollouts to import\n  Codex: 2 of 2 rollouts, 20 chunks\n"
        );
    }

    #[test]
    fn nothing_to_import_prints_nothing() {
        let mut printer = plain();
        printer.report(at(0.0), p(0, 0, 0));
        printer.finish();
        assert_eq!(text(&printer), "");
    }

    #[test]
    fn a_plain_stream_has_no_carriage_returns_or_escapes() {
        let mut printer = plain();
        printer.report(at(0.0), p(0, 3, 0));
        printer.report(at(5.0), p(1, 3, 100));
        printer.report(at(6.0), p(3, 3, 300));
        printer.finish();
        let out = text(&printer);
        assert!(!out.contains('\r') && !out.contains('\x1b'), "{out:?}");
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn a_terminal_redraws_one_line_and_ends_it_with_a_newline() {
        let mut printer = ProgressPrinter::new(Vec::new(), true, "Codex", "rollouts");
        printer.report(at(0.0), p(0, 2, 0));
        printer.report(at(4.0), p(1, 2, 50));
        printer.report(at(5.0), p(2, 2, 90));
        let out = String::from_utf8(printer.out.clone()).unwrap();
        assert_eq!(
            out,
            "  Codex: 2 rollouts to import\n\
             \r\x1b[2K  Codex: 1 of 2 rollouts, 50 chunks\
             \r\x1b[2K  Codex: 2 of 2 rollouts, 90 chunks\n"
        );
        // Closing a finished line twice adds nothing.
        printer.finish();
        assert_eq!(printer.out.len(), out.len());
    }

    #[test]
    fn finish_closes_a_line_left_open_by_an_interrupted_import() {
        let mut printer = ProgressPrinter::new(Vec::new(), true, "Codex", "rollouts");
        printer.report(at(0.0), p(0, 5, 0));
        printer.report(at(4.0), p(2, 5, 70));
        assert!(!printer.out.ends_with(b"\n"));
        printer.finish();
        assert!(printer.out.ends_with(b"\n"));
    }
}
