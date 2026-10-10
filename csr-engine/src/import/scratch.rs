//! Scratch scripts: code Claude writes inline in Bash tool calls.
//!
//! Heredocs (`cat > /tmp/x.py <<'EOF'`, `python3 - <<EOF`, `tee`) and one-liners
//! (`python3 -c`, `node -e`) never exist as files for long, and the plain
//! import keeps only the first 120 characters of a command. This module pulls
//! the script body out of the transcript, parses it with the normal code-graph
//! extractor under a synthetic path (`scratch/<session8>/<label>-<hash>.<ext>`),
//! and keeps the body in `scratch_scripts`.
//!
//! Three things build on the stored rows:
//! * P1: the scratch functions become `code_nodes` rows like any file.
//! * P2: a scratch function with the same name and body as a repo function of
//!   the same project is shown by `csr_why`: as "became final" only when the
//!   repo function's first conversation began after the scratch was written,
//!   otherwise as a plain "same body as".
//! * P3: a plain frequency table of languages and imports for `status`.
//!
//! No model is involved anywhere. CSR's own payloads stay out: any body that
//! `contamination_reason` flags is dropped, and so is any body that looks like
//! it carries a credential.

use anyhow::Result;
use regex::Regex;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::OnceLock;

use crate::storage::Storage;

/// Longest script body we will keep or parse (matches the hook's file cap).
const MAX_BODY_BYTES: usize = 256 * 1024;
/// Longest transcript line we will JSON-parse.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Code-graph project namespace for a project's scratch nodes. Scratch nodes
/// live apart from the repo's own nodes so the edge resolver never sees a
/// scratch `load_rows` next to the repo's `load_rows` (that would turn every
/// "became final" pair into an ambiguous name and unbind the repo's edges).
pub fn scratch_project(project: &str) -> String {
    format!("scratch:{project}")
}

/// One script found inside a Bash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// File stem the script was written to, or `inline` for stdin and `-c`.
    pub label: String,
    /// Extension the extractor understands: py, js, ts, tsx, rs or go.
    pub ext: &'static str,
    pub body: String,
}

/// What one transcript pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IndexStats {
    pub scripts_seen: usize,
    pub scripts_new: usize,
    pub nodes: usize,
}

/// Create the `scratch_scripts` table (idempotent, read-only when it exists).
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS scratch_scripts (
            id         TEXT PRIMARY KEY,
            session_id TEXT NOT NULL DEFAULT '',
            conv_id    TEXT NOT NULL DEFAULT '',
            project    TEXT NOT NULL DEFAULT '',
            lang       TEXT NOT NULL DEFAULT '',
            file       TEXT NOT NULL,
            body_hash  TEXT NOT NULL DEFAULT '',
            body       TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_scratch_scripts_file ON scratch_scripts(file);
        CREATE INDEX IF NOT EXISTS idx_scratch_scripts_conv ON scratch_scripts(conv_id);",
    )?;
    Ok(())
}

fn ext_for_path(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "py" => Some("py"),
        "js" | "mjs" | "jsx" => Some("js"),
        "ts" => Some("ts"),
        "tsx" => Some("tsx"),
        "rs" => Some("rs"),
        "go" => Some("go"),
        _ => None,
    }
}

fn ext_for_interpreter(token: &str) -> Option<&'static str> {
    let base = token.rsplit('/').next().unwrap_or(token);
    if base.starts_with("python") {
        Some("py")
    } else if base == "node" || base == "bun" || base == "deno" {
        Some("js")
    } else if base == "tsx" || base == "ts-node" {
        Some("ts")
    } else {
        None
    }
}

fn file_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        "inline".to_string()
    } else {
        clean
    }
}

/// Text that looks like it carries a credential: never stored or parsed.
fn looks_secret(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "secret",
        "password",
        "passwd",
        "token=",
        "token =",
        "bearer ",
        "-----begin",
        "sk-",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// The redirect or tee target on a heredoc line, if any.
fn redirect_target(line: &str) -> Option<String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        let target = if *tok == ">" || *tok == ">>" {
            tokens.get(i + 1).copied()
        } else if let Some(rest) = tok.strip_prefix(">>").or_else(|| tok.strip_prefix('>')) {
            (!rest.is_empty() && !rest.starts_with('&')).then_some(rest)
        } else if *tok == "tee" {
            tokens[i + 1..]
                .iter()
                .copied()
                .find(|t| !t.starts_with('-'))
        } else {
            None
        };
        if let Some(t) = target {
            let t = t.trim_matches(|c| c == '\'' || c == '"');
            if ext_for_path(t).is_some() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Interpreter reading its program from the heredoc (`python3 - <<EOF`,
/// `node <<EOF`). `python3 run.py <<EOF` feeds data to a script, not code.
fn stdin_interpreter(line: &str) -> Option<&'static str> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        let Some(ext) = ext_for_interpreter(tok) else {
            continue;
        };
        let next = tokens[i + 1..]
            .iter()
            .find(|t| !(t.starts_with('-') && t.len() > 1));
        // The first non-flag word must be `-` or the heredoc itself.
        match next {
            None => return Some(ext),
            Some(t) if *t == "-" || t.starts_with("<<") => return Some(ext),
            _ => {}
        }
    }
    None
}

/// Heredoc marker right after a `<<` operator (`rest` starts after the two
/// `<`): `EOF` for `<<'EOF'`, `<<-EOF`, `<< EOF`. `None` for a here-string
/// (`<<<`) or a `<<` with no word after it (a shift operator).
fn marker_after(rest: &str) -> Option<String> {
    if rest.starts_with('<') {
        return None;
    }
    let rest = rest.strip_prefix('-').unwrap_or(rest).trim_start();
    let rest = rest.trim_start_matches(['\'', '"', '\\']);
    let marker: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!marker.is_empty()).then_some(marker)
}

/// Where a shell `<<` heredoc opens on `line`, outside any quote, given the
/// quote still open from earlier lines. Returns the marker (if the line opens
/// one) and the quote left open at the end of the line.
fn scan_line(line: &str, mut quote: Option<char>) -> (Option<String>, Option<char>) {
    let mut marker: Option<String> = None;
    let mut prev_space = true;
    let mut chars = line.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                }
            }
            Some(q) => {
                // inside "..." (or `...`): a backslash escapes the next char
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\\' => {
                    chars.next();
                }
                '\'' | '"' | '`' => quote = Some(c),
                // a comment runs to the end of the line
                '#' if prev_space => break,
                '<' if marker.is_none() && line[at + 1..].starts_with('<') => {
                    marker = marker_after(&line[at + 2..]);
                    chars.next();
                }
                _ => {}
            },
        }
        prev_space = c.is_whitespace() || (quote.is_none() && matches!(c, ';' | '&' | '|' | '('));
    }
    (marker, quote)
}

/// One heredoc script found by [`scan_heredocs`].
struct Heredoc {
    opener: String,
    body: String,
}

/// Most lines one command may make the heredoc scanner visit, so no input can
/// turn the scan quadratic.
const MAX_SCAN_STEPS: usize = 200_000;

/// Closed heredocs in `command`, with the number of lines visited. A `<<`
/// inside a quoted string (a shift in `python3 -c "x << 1"`) opens nothing; a
/// marker that failed to close once cannot close later; bodies longer than
/// [`MAX_BODY_BYTES`] are skipped without being copied.
fn scan_heredocs(command: &str) -> (Vec<Heredoc>, usize) {
    let lines: Vec<&str> = command.lines().collect();
    let mut found = Vec::new();
    let mut unclosed: Vec<String> = Vec::new();
    let mut steps = 0usize;
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < lines.len() && steps < MAX_SCAN_STEPS {
        steps += 1;
        let (marker, open) = scan_line(lines[i], quote);
        quote = open;
        let Some(marker) = marker else {
            i += 1;
            continue;
        };
        // The body starts on the next physical line; a quote still open at the
        // end of this one means the operator was not a heredoc after all.
        if quote.is_some() || unclosed.contains(&marker) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut bytes = 0usize;
        let mut closed = false;
        while j < lines.len() && steps < MAX_SCAN_STEPS {
            steps += 1;
            if lines[j].trim() == marker {
                closed = true;
                break;
            }
            bytes += lines[j].len() + 1;
            j += 1;
        }
        if !closed {
            unclosed.push(marker);
            i += 1;
            continue;
        }
        if bytes <= MAX_BODY_BYTES {
            let mut body = String::with_capacity(bytes);
            for line in &lines[i + 1..j] {
                body.push_str(line);
                body.push('\n');
            }
            found.push(Heredoc {
                opener: lines[i].to_string(),
                body,
            });
        }
        i = j + 1;
        quote = None;
    }
    (found, steps)
}

fn quoted_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?:^|[\s;&|(])([\w./+-]*(?:python[0-9.]*|node|bun|deno))\s+(?:-[A-Za-z]+\s+)*?(?:-c|-e|--eval)\s+(['"])"#)
            .expect("quoted script regex")
    })
}

/// Read a shell-quoted string starting just after its opening quote: the
/// unescaped body, and the byte offset in `rest` just past the closing quote
/// (the body of a double-quoted string is shorter than the text it came from,
/// so its length is not that offset).
fn read_quoted(rest: &str, quote: char) -> Option<(String, usize)> {
    if quote == '\'' {
        return rest
            .find('\'')
            .map(|end| (rest[..end].to_string(), end + 1));
    }
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((_, c)) = chars.next() {
        match c {
            '"' => {
                let end = chars.next().map_or(rest.len(), |(at, _)| at);
                return Some((out, end));
            }
            '\\' => match chars.next() {
                Some((_, n @ ('"' | '\\' | '$' | '`'))) => out.push(n),
                Some((_, n)) => {
                    out.push('\\');
                    out.push(n);
                }
                None => return None,
            },
            _ => out.push(c),
        }
    }
    None
}

/// Every parseable script in one Bash command, in order, without duplicates.
pub fn extract_scripts(command: &str) -> Vec<Script> {
    let mut found: Vec<Script> = Vec::new();
    let mut push = |label: String, ext: &'static str, body: String| {
        let trimmed = body.trim_end().to_string();
        if trimmed.trim().is_empty()
            || trimmed.len() > MAX_BODY_BYTES
            || looks_secret(&trimmed)
            || crate::import::contamination_reason(&trimmed).is_some()
        {
            return;
        }
        if !found.iter().any(|s| s.body == trimmed) {
            found.push(Script {
                label,
                ext,
                body: trimmed,
            });
        }
    };

    // Heredocs.
    for heredoc in scan_heredocs(command).0 {
        if let Some(path) = redirect_target(&heredoc.opener) {
            if let Some(ext) = ext_for_path(&path) {
                push(file_stem(&path), ext, heredoc.body);
            }
        } else if let Some(ext) = stdin_interpreter(&heredoc.opener) {
            push("inline".to_string(), ext, heredoc.body);
        }
    }

    // `python3 -c '...'`, `node -e "..."`.
    let re = quoted_regex();
    let mut from = 0;
    while let Some(caps) = re.captures(&command[from..]) {
        let whole = caps.get(0).expect("whole match");
        let quote = caps[2].chars().next().expect("quote char");
        let start = from + whole.end();
        let Some(ext) = ext_for_interpreter(&caps[1]) else {
            from = start;
            continue;
        };
        match read_quoted(&command[start..], quote) {
            Some((body, consumed)) => {
                push("inline".to_string(), ext, body);
                from = start + consumed.max(1);
            }
            None => from = start,
        }
        if from >= command.len() {
            break;
        }
    }

    found
}

fn short_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn full_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

fn lang_for_ext(ext: &str) -> Option<ast_grep_language::SupportLang> {
    crate::extraction::ast_analysis::lang_from_path_str(&format!("x.{ext}"))
}

/// Index the scratch scripts in a command and return the nodes written.
#[allow(clippy::too_many_arguments)]
fn index_command(
    storage: &Storage,
    command: &str,
    tool_use_id: &str,
    conv_id: &str,
    session_id: &str,
    project: &str,
    created_at: &str,
    stats: &mut IndexStats,
) -> Result<()> {
    let session8: String = session_id.chars().filter(|c| *c != '-').take(8).collect();
    // Extraction is pure string work, but an import must never be taken down by
    // a hostile or malformed command: a panic here costs only this command.
    let scripts = std::panic::catch_unwind(|| extract_scripts(command)).unwrap_or_else(|_| {
        tracing::warn!("scratch extraction panicked; command skipped");
        Vec::new()
    });
    for (n, script) in scripts.into_iter().enumerate() {
        stats.scripts_seen += 1;
        let id = full_hash(&format!("{conv_id}|{tool_use_id}|{n}"));
        let exists = storage.with_connection(|conn| {
            Ok(conn
                .query_row(
                    "SELECT 1 FROM scratch_scripts WHERE id = ?1",
                    params![id],
                    |_| Ok(()),
                )
                .is_ok())
        })?;
        if exists {
            continue;
        }
        let Some(lang) = lang_for_ext(script.ext) else {
            continue;
        };
        let file = format!(
            "scratch/{session8}/{}-{}.{}",
            script.label,
            short_hash(&script.body),
            script.ext
        );
        let graph_project = scratch_project(project);
        let fragment = crate::extraction::codegraph::extract_graph_fragment(
            &script.body,
            lang,
            &file,
            &graph_project,
            &graph_project,
            conv_id,
            session_id,
        );
        // Prose, JSON and other non-code heredocs parse with errors: skip them.
        if !fragment.parse_clean || fragment.nodes.is_empty() {
            continue;
        }
        for node in &fragment.nodes {
            let mut node = node.clone();
            node.repo_root = None;
            storage.upsert_code_node(&node)?;
        }
        storage.replace_code_file_edges(&graph_project, &file, &fragment.edges)?;
        storage.with_connection(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO scratch_scripts
                    (id, session_id, conv_id, project, lang, file, body_hash, body, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    session_id,
                    conv_id,
                    project,
                    script.ext,
                    file,
                    full_hash(&script.body),
                    script.body,
                    created_at
                ],
            )?;
            Ok(())
        })?;
        stats.scripts_new += 1;
        stats.nodes += fragment.nodes.len();
    }
    Ok(())
}

/// Walk one conversation transcript and index every Bash scratch script.
/// Safe to run again on a growing file: scripts already stored are skipped.
pub fn index_transcript(
    storage: &Storage,
    path: &Path,
    conv_id: &str,
    project: &str,
) -> Result<IndexStats> {
    let mut stats = IndexStats::default();
    let file = std::fs::File::open(path)?;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        // Cheap prefilter before the JSON parse: a Bash call carrying a script.
        if line.len() > MAX_LINE_BYTES
            || !line.contains("\"Bash\"")
            || !(line.contains("<<")
                || line.contains("\\\"-c\\\"")
                || line.contains(" -c ")
                || line.contains(" -e "))
        {
            continue;
        }
        let Ok(parsed) = sonic_rs::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if parsed.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let session_id = parsed
            .get("sessionId")
            .and_then(|s| s.as_str())
            .unwrap_or(conv_id)
            .to_string();
        let created_at = parsed
            .get("timestamp")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let Some(content) = parsed
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for item in content {
            if item.get("type").and_then(|t| t.as_str()) != Some("tool_use")
                || item.get("name").and_then(|n| n.as_str()) != Some("Bash")
            {
                continue;
            }
            let Some(command) = item
                .get("input")
                .and_then(|i| i.get("command"))
                .and_then(|c| c.as_str())
            else {
                continue;
            };
            let tool_use_id = item.get("id").and_then(|i| i.as_str()).unwrap_or("");
            if let Err(e) = index_command(
                storage,
                command,
                tool_use_id,
                conv_id,
                &session_id,
                project,
                &created_at,
                &mut stats,
            ) {
                tracing::warn!(error = %e, "scratch script skipped");
            }
        }
    }
    Ok(stats)
}

/// P2: one scratch/repo match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    pub name: String,
    pub repo_file: String,
    pub scratch_file: String,
    pub session8: String,
    /// `YYYY-MM-DD`, empty when the transcript had no timestamp.
    pub date: String,
    /// The repo function was first seen in a conversation that began after the
    /// scratch script was written. Without that, only the bodies are known to
    /// match: the repo function may be older than the scratch.
    pub followed_scratch: bool,
}

impl Promotion {
    /// One plain line for `csr_why`.
    pub fn line(&self) -> String {
        let when = if self.date.is_empty() {
            String::new()
        } else {
            format!(" on {}", self.date)
        };
        if self.followed_scratch {
            format!(
                "became final: {} in {} was drafted as scratch in session {}{} (same body)",
                self.name, self.repo_file, self.session8, when
            )
        } else {
            format!(
                "same body as {} in {}: drafted as scratch in session {}{}",
                self.name, self.repo_file, self.session8, when
            )
        }
    }
}

/// Matching pairs: a repo function and a scratch function of the SAME project
/// with the same kind, name and body hash. `followed_scratch` (column 6) is 1
/// only when the repo function's first conversation is another conversation
/// than the scratch's and began after the scratch script was written.
const PROMOTION_PAIRS: &str = "
    FROM code_nodes r
    JOIN code_nodes s ON s.name = r.name AND s.body_hash = r.body_hash
                     AND s.kind = r.kind AND s.project = 'scratch:' || r.project
    LEFT JOIN scratch_scripts ss ON ss.file = s.file
    WHERE r.kind IN ('function', 'method')
      AND r.file NOT LIKE 'scratch/%'
      AND s.file LIKE 'scratch/%'";

const FOLLOWED_SCRATCH: &str = "CASE WHEN ss.created_at <> '' AND r.first_conv_id <> ''
         AND ss.conv_id <> r.first_conv_id
         AND julianday((SELECT MIN(c.timestamp) FROM chunks c
                         WHERE c.conversation_id = r.first_conv_id)) > julianday(ss.created_at)
       THEN 1 ELSE 0 END";

/// Scratch functions whose name and body hash match a repo function of the
/// same project. `names` limits the search to those repo function names
/// (empty = none).
pub fn promotions_for_names(conn: &Connection, names: &[String]) -> Result<Vec<Promotion>> {
    let mut out: Vec<Promotion> = Vec::new();
    if names.is_empty() {
        return Ok(out);
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT r.name, r.file, s.file, COALESCE(ss.session_id, ''), COALESCE(ss.created_at, ''),
                {FOLLOWED_SCRATCH}
         {PROMOTION_PAIRS}
           AND r.name = ?1
         ORDER BY 6 DESC, ss.created_at ASC
         LIMIT 3"
    ))?;
    for name in names {
        let rows = stmt.query_map(params![name], |row| {
            let session: String = row.get(3)?;
            let created: String = row.get(4)?;
            Ok(Promotion {
                name: row.get(0)?,
                repo_file: row.get(1)?,
                scratch_file: row.get(2)?,
                session8: session.chars().filter(|c| *c != '-').take(8).collect(),
                date: created.chars().take(10).collect(),
                followed_scratch: row.get::<_, i64>(5)? == 1,
            })
        })?;
        for row in rows {
            let p = row?;
            if !out
                .iter()
                .any(|q| q.name == p.name && q.scratch_file == p.scratch_file)
            {
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// P3: the frequency table shown under `scratch` in `csr-engine status`.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScratchStatus {
    pub scripts: i64,
    pub functions: i64,
    pub by_language: BTreeMap<String, i64>,
    /// Most-used imports across scratch scripts, most frequent first.
    pub top_imports: Vec<(String, i64)>,
    /// Scratch functions that later appeared verbatim in a repo function of
    /// the same project (the repo function first seen after the scratch ran).
    pub became_final: i64,
}

pub fn status(conn: &Connection) -> Result<ScratchStatus> {
    let mut out = ScratchStatus::default();
    let has_table: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='scratch_scripts'",
            [],
            |_| Ok(()),
        )
        .is_ok();
    if !has_table {
        return Ok(out);
    }
    out.scripts = conn.query_row("SELECT COUNT(*) FROM scratch_scripts", [], |r| r.get(0))?;
    out.functions = conn.query_row(
        "SELECT COUNT(*) FROM code_nodes WHERE file LIKE 'scratch/%' AND kind IN ('function','method')",
        [],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare("SELECT lang, COUNT(*) FROM scratch_scripts GROUP BY lang")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (lang, n) = row?;
        out.by_language.insert(lang, n);
    }
    // Imports are `imports` edges out of each scratch module; the target is a
    // `name:<symbol>` placeholder until the resolver repoints it at a node.
    let mut stmt = conn.prepare(
        "SELECT COALESCE(n.name, REPLACE(e.dst_id, 'name:', '')) AS sym, COUNT(*) AS c
         FROM code_edges e LEFT JOIN code_nodes n ON n.id = e.dst_id
         WHERE e.src_file LIKE 'scratch/%' AND e.kind = 'imports'
         GROUP BY sym ORDER BY c DESC, sym ASC LIMIT 10",
    )?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        out.top_imports.push(row?);
    }
    out.became_final = conn.query_row(
        &format!("SELECT COUNT(DISTINCT s.id) {PROMOTION_PAIRS} AND {FOLLOWED_SCRATCH} = 1"),
        [],
        |r| r.get(0),
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heredoc_to_file_keeps_name_and_ext() {
        let cmd = "cat > /tmp/probe.py <<'EOF'\nimport json\ndef f():\n    return 1\nEOF\npython3 /tmp/probe.py";
        let s = extract_scripts(cmd);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].label, "probe");
        assert_eq!(s[0].ext, "py");
        assert!(s[0].body.contains("def f()"));
    }

    #[test]
    fn python_stdin_heredoc_is_inline() {
        let cmd = "cd /x && python3 - <<EOF\nimport os\nprint(os.getcwd())\nEOF";
        let s = extract_scripts(cmd);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].label, "inline");
        assert_eq!(s[0].ext, "py");
    }

    #[test]
    fn tee_and_node_forms() {
        let cmd = "tee /tmp/a.js <<'X' > /dev/null\nconst a = 1;\nfunction g(){ return a }\nX";
        let s = extract_scripts(cmd);
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].label.as_str(), s[0].ext), ("a", "js"));
        let s = extract_scripts("node -e 'function h(){return 2}; console.log(h())'");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].ext, "js");
        assert!(s[0].body.starts_with("function h"));
    }

    #[test]
    fn dash_c_double_quotes_unescape() {
        let s = extract_scripts("python3 -c \"import sys\\nprint(\\\"hi\\\")\"");
        assert_eq!(s.len(), 1);
        assert!(s[0].body.contains("print(\"hi\")"));
    }

    #[test]
    fn shell_heredocs_secrets_and_csr_payloads_are_skipped() {
        assert!(extract_scripts("cat > /tmp/x.sh <<'EOF'\necho hi\nEOF").is_empty());
        assert!(extract_scripts("cat <<EOF\nplain text\nEOF").is_empty());
        assert!(extract_scripts("python3 - <<EOF\nAPI_KEY = 'abc'\nprint(1)\nEOF").is_empty());
        assert!(extract_scripts("python3 - <<EOF\nprint('x')\nEOF").len() == 1);
        // CSR's own injected blocks stay suppressed
        assert!(extract_scripts(
            "python3 - <<EOF\nprint('<system-reminder>CSR PICKUP — recall</system-reminder>')\nEOF"
        )
        .is_empty());
        // unclosed heredoc is not a script
        assert!(extract_scripts("python3 - <<EOF\nprint(1)\n").is_empty());
    }

    #[test]
    fn script_fed_stdin_data_is_not_code() {
        // `python3 run.py <<EOF` feeds input to a script: the body is data.
        assert!(extract_scripts("python3 run.py <<EOF\nanswer\nEOF").is_empty());
        assert!(extract_scripts("cat <<EOF | python3 tool.py --x\nfoo()\nEOF").is_empty());
        // Flags before `-`, and a heredoc piped into the interpreter, are code.
        assert_eq!(
            extract_scripts("python3 -I - <<EOF\nprint(1)\nEOF").len(),
            1
        );
        assert_eq!(
            extract_scripts("cat <<EOF | python3\nprint(2)\nEOF").len(),
            1
        );
    }

    #[test]
    fn unclosed_heredoc_does_not_hide_later_scripts() {
        let cmd =
            "python3 -c 'x = 1 << 4'\ncat > /tmp/b.py <<'EOF'\ndef later():\n    return 3\nEOF";
        let s = extract_scripts(cmd);
        assert!(s.iter().any(|x| x.label == "b"), "{s:?}");
    }

    fn temp_storage() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("t.db")).unwrap();
        (dir, storage)
    }

    fn write_transcript(dir: &Path, commands: &[(&str, &str)]) -> std::path::PathBuf {
        let path = dir.join("abcd1234-0000-0000-0000-000000000000.jsonl");
        let mut text = String::new();
        for (i, (id, cmd)) in commands.iter().enumerate() {
            let rec = serde_json::json!({
                "type": "assistant",
                "sessionId": "abcd1234-0000-0000-0000-000000000000",
                "timestamp": format!("2026-09-30T10:0{i}:00Z"),
                "message": {"content": [
                    {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": cmd}}
                ]}
            });
            text.push_str(&rec.to_string());
            text.push('\n');
        }
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn transcript_scripts_become_nodes_and_are_idempotent() {
        let (dir, storage) = temp_storage();
        let path = write_transcript(
            dir.path(),
            &[
                ("t1", "python3 - <<'EOF'\nimport json\nimport sqlite3\ndef load_rows(p):\n    return json.load(open(p))\nEOF"),
                ("t2", "echo hello"),
                ("t3", "cat > /tmp/data.py <<'EOF'\nthis is { not code at all ((\nEOF"),
            ],
        );
        let first = index_transcript(&storage, &path, "conv-a", "proj").unwrap();
        assert_eq!(first.scripts_new, 1, "{first:?}");
        assert!(first.nodes >= 2);
        let again = index_transcript(&storage, &path, "conv-a", "proj").unwrap();
        assert_eq!(again.scripts_new, 0);

        let (files, body): (i64, String) = storage
            .with_connection(|c| {
                let n = c.query_row(
                    "SELECT COUNT(DISTINCT file) FROM code_nodes WHERE file LIKE 'scratch/abcd1234/%'",
                    [],
                    |r| r.get(0),
                )?;
                let b = c.query_row("SELECT body FROM scratch_scripts", [], |r| r.get(0))?;
                Ok((n, b))
            })
            .unwrap();
        assert_eq!(files, 1);
        assert!(body.contains("def load_rows"));
        let has_fn: i64 = storage
            .with_connection(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM code_nodes WHERE name='load_rows' AND file LIKE 'scratch/%'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(has_fn, 1);
    }

    const ROWS_BODY: &str =
        "import json\nimport sqlite3\ndef load_rows(p):\n    return json.load(open(p))\n";

    /// Index `ROWS_BODY` as a scratch script (conv-a, 2026-09-30T10:00:00Z) and
    /// land the same function in a repo file of `repo_project`, first seen in
    /// conversation `conv-b`.
    fn scratch_then_repo(dir: &Path, storage: &Storage, repo_project: &str) {
        let path = write_transcript(
            dir,
            &[("t1", &format!("python3 - <<'EOF'\n{ROWS_BODY}EOF"))],
        );
        index_transcript(storage, &path, "conv-a", "proj").unwrap();
        let frag = crate::extraction::codegraph::extract_graph_fragment_for_file(
            ROWS_BODY,
            "src/rows.py",
            repo_project,
            repo_project,
            "conv-b",
            "sess-b",
        );
        for n in &frag.nodes {
            storage.upsert_code_node(n).unwrap();
        }
    }

    fn conv_b_starts_at(storage: &Storage, timestamp: &str) {
        let chunk = crate::import::ConversationChunk {
            id: "chunk-b".into(),
            conversation_id: "conv-b".into(),
            project_name: "proj".into(),
            timestamp: timestamp.into(),
            content: "landing load_rows in src/rows.py".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
    }

    fn promos(storage: &Storage) -> Vec<Promotion> {
        storage
            .with_connection(|c| promotions_for_names(c, &["load_rows".to_string()]))
            .unwrap()
    }

    #[test]
    fn same_body_is_only_claimed_as_final_when_the_repo_function_came_after() {
        let (dir, storage) = temp_storage();
        scratch_then_repo(dir.path(), &storage, "proj");

        // The repo function's first conversation has no record of when it
        // began: the bodies match, the order is unknown.
        let unknown = promos(&storage);
        assert_eq!(unknown.len(), 1, "{unknown:?}");
        assert!(!unknown[0].followed_scratch);
        assert_eq!(unknown[0].repo_file, "src/rows.py");
        assert_eq!(unknown[0].session8, "abcd1234");
        assert_eq!(unknown[0].date, "2026-09-30");
        let line = unknown[0].line();
        assert!(line.starts_with("same body as load_rows"), "{line}");
        assert!(!line.contains("became final"), "{line}");
        assert_eq!(storage.with_connection(status).unwrap().became_final, 0);

        // The repo function came first: still only the same body.
        conv_b_starts_at(&storage, "2026-09-29T09:00:00Z");
        let before = promos(&storage);
        assert!(!before[0].followed_scratch, "{before:?}");
        assert!(!before[0].line().contains("became final"));
        assert_eq!(storage.with_connection(status).unwrap().became_final, 0);
    }

    #[test]
    fn a_repo_function_first_seen_after_the_scratch_became_final() {
        let (dir, storage) = temp_storage();
        scratch_then_repo(dir.path(), &storage, "proj");
        conv_b_starts_at(&storage, "2026-10-02T09:00:00Z");

        let after = promos(&storage);
        assert_eq!(after.len(), 1, "{after:?}");
        assert!(after[0].followed_scratch);
        assert!(after[0]
            .line()
            .contains("became final: load_rows in src/rows.py was drafted as scratch in session abcd1234 on 2026-09-30"));
        assert!(storage
            .with_connection(|c| promotions_for_names(c, &["other".to_string()]))
            .unwrap()
            .is_empty());

        // Scratch nodes sit in their own namespace: inside the repo project the
        // name stays unique, so the resolver's unique-name binding still works.
        let (in_repo, in_scratch): (i64, i64) = storage
            .with_connection(|c| {
                let q = "SELECT COUNT(*) FROM code_nodes WHERE name = 'load_rows' AND project = ?1";
                Ok((
                    c.query_row(q, ["proj"], |r| r.get(0))?,
                    c.query_row(q, [scratch_project("proj")], |r| r.get(0))?,
                ))
            })
            .unwrap();
        assert_eq!((in_repo, in_scratch), (1, 1));

        let st = storage.with_connection(status).unwrap();
        assert_eq!(st.scripts, 1);
        assert_eq!(st.by_language.get("py"), Some(&1));
        assert_eq!(st.became_final, 1);
        assert!(
            st.top_imports.iter().any(|(n, _)| n.contains("json")),
            "{:?}",
            st.top_imports
        );
    }

    #[test]
    fn a_repo_function_of_another_project_never_matches_the_scratch() {
        let (dir, storage) = temp_storage();
        scratch_then_repo(dir.path(), &storage, "other-project");
        conv_b_starts_at(&storage, "2026-10-02T09:00:00Z");
        assert!(promos(&storage).is_empty());
        assert_eq!(storage.with_connection(status).unwrap().became_final, 0);
    }

    #[test]
    fn a_closing_quote_after_an_escaped_quote_and_unicode_does_not_panic() {
        // The unescaped body is shorter than the text it came from: advancing
        // by its length landed inside the multi-byte `é`.
        let cmd = "python3 -c \"print('\\\"') # é\"\nnode -e 'function ünï(){return 1}'";
        let s = extract_scripts(cmd);
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!(s[0].body, "print('\"') # é");
        assert!(s[1].body.contains("ünï"));
        // and a lone escaped quote followed by wide characters, many times over
        let many = "python3 -c \"print('\\\"') # 日本語é\"\n".repeat(50);
        let _ = extract_scripts(&many);
        let _ = extract_scripts("python3 -c \"é\\\"");
    }

    #[test]
    fn shift_expressions_inside_a_quoted_script_open_no_heredoc_and_cost_one_pass() {
        let mut body = String::from("x = 1\n");
        for _ in 0..8_000 {
            body.push_str("x = x << 1\n");
        }
        let cmd = format!("python3 -c \"{body}print(x)\"\ncat > /tmp/b.py <<'EOF'\ndef later():\n    return 3\nEOF");
        let lines = cmd.lines().count();
        let (heredocs, steps) = scan_heredocs(&cmd);
        assert_eq!(heredocs.len(), 1, "only the real heredoc");
        assert!(heredocs[0].body.contains("def later"));
        assert!(steps <= lines + 8, "{steps} steps for {lines} lines");
        assert!(extract_scripts(&cmd).iter().any(|x| x.label == "b"));
    }

    #[test]
    fn unquoted_shifts_and_repeated_unclosed_markers_stay_linear() {
        // Unquoted `<< 1` shifts open nothing that closes. The first failed
        // search proves every later one for that marker fails too.
        let cmd = "echo $((x << 1))\n".repeat(8_000);
        let lines = cmd.lines().count();
        let (heredocs, steps) = scan_heredocs(&cmd);
        assert!(heredocs.is_empty());
        assert!(steps <= 2 * lines + 8, "{steps} steps for {lines} lines");
        // distinct unclosed markers are still bounded by the global step cap
        let many: String = (0..30_000).map(|i| format!("cat <<M{i}\n")).collect();
        let (_, steps) = scan_heredocs(&many);
        assert!(steps <= MAX_SCAN_STEPS + 1, "{steps}");
    }

    #[test]
    fn an_oversized_heredoc_is_skipped_without_hiding_the_next_one() {
        let big = "x = 1\n".repeat(MAX_BODY_BYTES / 6 + 10);
        let cmd =
            format!("python3 - <<'A'\n{big}A\ncat > /tmp/ok.py <<'B'\ndef ok():\n    return 1\nB");
        let s = extract_scripts(&cmd);
        assert_eq!(s.len(), 1, "{}", s.len());
        assert_eq!(s[0].label, "ok");
    }
}
