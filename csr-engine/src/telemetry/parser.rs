//! Streaming parser for `~/.claude-self-reflect/hook-timing.log`.
//!
//! Recognised line shapes (everything else is silently skipped):
//!
//!   2026-05-31T02:53:07Z CSR hook stop [claude-self-reflect]: stdin=0ms setup=0ms hook=54ms flush=58ms total=113ms
//!   2026-05-31T02:53:33Z CSR startup: storage=0ms embed=48ms cache_load=38ms total=87ms (17737 chunks, cached)
//!   2026-05-31T02:53:36Z CSR startup: storage=0ms embed=52ms vectors=15ms hnsw=13661ms dump=24ms total=13753ms (17737 chunks, rebuilt)

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub enum Entry {
    Hook {
        ts: DateTime<Utc>,
        name: String,
        project: Option<String>,
        total_ms: u64,
        hook_ms: u64,
    },
    Startup {
        ts: DateTime<Utc>,
        total_ms: u64,
        rebuilt: bool,
        chunks: u64,
    },
}

impl Entry {
    pub fn ts(&self) -> DateTime<Utc> {
        match self {
            Entry::Hook { ts, .. } | Entry::Startup { ts, .. } => *ts,
        }
    }
}

/// Read the log file, returning entries within `cutoff` and total scanned line count.
///
/// `cutoff = None` means "all entries". If the file is missing, returns an empty vec.
pub fn read_log(path: &Path, cutoff: Option<DateTime<Utc>>) -> Result<(Vec<Entry>, usize)> {
    if !path.exists() {
        return Ok((Vec::new(), 0));
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let mut entries = Vec::new();
    let mut scanned = 0usize;
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        scanned += 1;
        let Some(entry) = parse_line(&line) else {
            continue;
        };
        if let Some(c) = cutoff {
            if entry.ts() < c {
                continue;
            }
        }
        entries.push(entry);
    }
    Ok((entries, scanned))
}

/// Parse one log line into an `Entry`, or `None` if unrecognised.
pub fn parse_line(line: &str) -> Option<Entry> {
    // Timestamp is always the first whitespace-delimited token.
    let (ts_str, rest) = line.split_once(' ')?;
    let ts: DateTime<Utc> = DateTime::parse_from_rfc3339(ts_str)
        .ok()?
        .with_timezone(&Utc);
    let rest = rest.trim_start();

    if let Some(rest) = rest.strip_prefix("CSR hook ") {
        return parse_hook(ts, rest);
    }
    if let Some(rest) = rest.strip_prefix("CSR startup:") {
        return parse_startup(ts, rest.trim_start());
    }
    None
}

fn parse_hook(ts: DateTime<Utc>, rest: &str) -> Option<Entry> {
    // "stop [claude-self-reflect]: stdin=0ms ... total=113ms"  or
    // "post-tool-use: stdin=0ms ... total=4ms"
    let (head, tail) = rest.split_once(':')?;
    let head = head.trim();
    let (name, project) = if let Some(open) = head.find('[') {
        let name = head[..open].trim().to_string();
        let close = head[open + 1..].find(']')?;
        let project = head[open + 1..open + 1 + close].to_string();
        (name, Some(project))
    } else {
        (head.to_string(), None)
    };
    if name.is_empty() {
        return None;
    }
    let total_ms = extract_ms(tail, "total=")?;
    let hook_ms = extract_ms(tail, "hook=").unwrap_or(total_ms);
    Some(Entry::Hook {
        ts,
        name,
        project,
        total_ms,
        hook_ms,
    })
}

fn parse_startup(ts: DateTime<Utc>, rest: &str) -> Option<Entry> {
    let total_ms = extract_ms(rest, "total=")?;
    // "(17737 chunks, cached)" or "(17737 chunks, rebuilt)"
    let (rebuilt, chunks) = match rest.rfind('(') {
        Some(i) => {
            let tail = &rest[i + 1..];
            let rebuilt = tail.contains("rebuilt");
            let chunks = tail
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            (rebuilt, chunks)
        }
        None => (false, 0),
    };
    Some(Entry::Startup {
        ts,
        total_ms,
        rebuilt,
        chunks,
    })
}

/// Parse a `<key>=<digits>ms` token out of free text. Returns the numeric ms part.
fn extract_ms(text: &str, key: &str) -> Option<u64> {
    let start = text.find(key)? + key.len();
    let tail = &text[start..];
    let end = tail
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(tail.len());
    if end == 0 {
        return None;
    }
    tail[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hook_with_project() {
        let line = "2026-05-31T02:53:07Z CSR hook stop [claude-self-reflect]: stdin=0ms setup=0ms hook=54ms flush=58ms total=113ms";
        let e = parse_line(line).unwrap();
        match e {
            Entry::Hook {
                name,
                project,
                total_ms,
                hook_ms,
                ..
            } => {
                assert_eq!(name, "stop");
                assert_eq!(project.as_deref(), Some("claude-self-reflect"));
                assert_eq!(total_ms, 113);
                assert_eq!(hook_ms, 54);
            }
            _ => panic!("expected hook"),
        }
    }

    #[test]
    fn parses_hook_without_project() {
        let line = "2026-05-31T02:53:07Z CSR hook post-tool-use: stdin=0ms setup=0ms hook=4ms flush=0ms total=4ms";
        let e = parse_line(line).unwrap();
        match e {
            Entry::Hook {
                name,
                project,
                total_ms,
                ..
            } => {
                assert_eq!(name, "post-tool-use");
                assert!(project.is_none());
                assert_eq!(total_ms, 4);
            }
            _ => panic!("expected hook"),
        }
    }

    #[test]
    fn parses_startup_cached() {
        let line = "2026-05-31T02:53:33Z CSR startup: storage=0ms embed=48ms cache_load=38ms total=87ms (17737 chunks, cached)";
        let e = parse_line(line).unwrap();
        match e {
            Entry::Startup {
                total_ms,
                rebuilt,
                chunks,
                ..
            } => {
                assert_eq!(total_ms, 87);
                assert!(!rebuilt);
                assert_eq!(chunks, 17737);
            }
            _ => panic!("expected startup"),
        }
    }

    #[test]
    fn parses_startup_rebuilt() {
        let line = "2026-05-31T02:53:36Z CSR startup: storage=0ms embed=52ms vectors=15ms hnsw=13661ms dump=24ms total=13753ms (17737 chunks, rebuilt)";
        let e = parse_line(line).unwrap();
        match e {
            Entry::Startup {
                total_ms, rebuilt, ..
            } => {
                assert_eq!(total_ms, 13753);
                assert!(rebuilt);
            }
            _ => panic!("expected startup"),
        }
    }

    #[test]
    fn ignores_unrecognised_lines() {
        assert!(parse_line("garbage line").is_none());
        assert!(
            parse_line("2026-05-31T02:53:07Z CSR session-start inject [foo]: stories=2").is_none()
        );
        assert!(parse_line("").is_none());
    }

    #[test]
    fn extract_ms_handles_edges() {
        assert_eq!(extract_ms("total=113ms", "total="), Some(113));
        assert_eq!(extract_ms("a=1ms b=22ms c=3ms", "b="), Some(22));
        assert_eq!(extract_ms("total=ms", "total="), None);
        assert_eq!(extract_ms("nothing", "total="), None);
    }
}
