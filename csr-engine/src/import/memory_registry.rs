//! Incremental scan of `~/.claude/projects/*/memory/*.md` into `memory_registry`.
//!
//! Data is NEVER embedded and NEVER injected into search — table + status only.
//! Call site lives in the daemon loop (a later stage, not this one).

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use sha2::{Digest, Sha256};

use crate::storage::queries::{self, MemoryRegistryRow};
use crate::storage::Storage;

const META_SCAN_GENERATION: &str = "memory_registry_scan_generation";
/// Soft read cap — files larger than this are metadata-only schema-misses.
const READ_CAP_BYTES: u64 = 262_144;

static WIKILINK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\[([^\]]+)\]\]").unwrap());

/// Aggregate counters returned by [`scan_memory_dirs`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryScanStats {
    pub files_seen: usize,
    pub upserted: usize,
    pub deleted: usize,
    pub schema_misses: usize,
}

/// Scan `<projects_root>/*/memory/*.md` into `memory_registry`.
///
/// Fail-soft per project/file: unreadable dirs are skipped (no stale-delete for
/// that project); malformed frontmatter still upserts a filename-fallback row
/// and bumps `aux_schema_miss:memory_frontmatter`. Never embeds or injects.
pub fn scan_memory_dirs(storage: &Storage, projects_root: &Path) -> Result<MemoryScanStats> {
    if !projects_root.exists() {
        return Ok(MemoryScanStats::default());
    }

    let generation = storage
        .get_meta(META_SCAN_GENERATION)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let current_scan_generation = generation + 1;

    let existing = load_existing_rows(storage)?;

    let mut files_seen = 0usize;
    let mut upserted = 0usize;
    let mut schema_misses = 0usize;
    let mut rows: Vec<MemoryRegistryRow> = Vec::new();
    let mut scanned_projects: Vec<String> = Vec::new();

    let project_entries = match fs::read_dir(projects_root) {
        Ok(e) => e,
        Err(_) => return Ok(MemoryScanStats::default()),
    };

    for entry in project_entries {
        let Ok(entry) = entry else {
            continue;
        };
        let project_path = entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let Some(project_name) = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };

        let memory_dir = project_path.join("memory");
        let mem_entries = match fs::read_dir(&memory_dir) {
            Ok(e) => e,
            // Missing or unreadable memory/ — skip this project entirely; do
            // NOT stale-delete its prior rows.
            Err(_) => continue,
        };

        scanned_projects.push(project_name.clone());

        for mem_entry in mem_entries {
            let Ok(mem_entry) = mem_entry else {
                continue;
            };
            let path = mem_entry.path();
            if !path.is_file() {
                continue;
            }
            if !is_md_extension(&path) {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // MEMORY.md is the index file, not a memory record.
            if file_name.eq_ignore_ascii_case("memory.md") {
                continue;
            }

            files_seen += 1;

            let file_path = path.to_string_lossy().into_owned();
            let meta = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => {
                    schema_misses += 1;
                    continue;
                }
            };
            let file_mtime = meta.mtime();

            // Unchanged on disk: re-upsert existing fields with bumped
            // last_seen_scan so stale-delete does not remove it. Do not count
            // toward upserted (no content change).
            if let Some(prev) = existing.get(&file_path) {
                if prev.file_mtime == file_mtime {
                    let mut touched = prev.clone();
                    touched.last_seen_scan = current_scan_generation;
                    rows.push(touched);
                    continue;
                }
            }

            let built = build_row_for_file(
                &path,
                &file_path,
                &project_name,
                file_mtime,
                meta.len(),
                current_scan_generation,
                &mut schema_misses,
            );
            rows.push(built);
            upserted += 1;
        }
    }

    let deleted = storage.with_transaction(|tx| {
        queries::upsert_memory_registry_batch(tx, &rows)?;
        let mut deleted = 0usize;
        for project in &scanned_projects {
            deleted += queries::delete_memory_registry_stale(tx, project, current_scan_generation)?;
        }
        queries::set_meta(
            tx,
            META_SCAN_GENERATION,
            &current_scan_generation.to_string(),
        )?;
        Ok(deleted)
    })?;

    if schema_misses > 0 {
        let _ = storage.bump_aux_counter_by("memory_frontmatter", schema_misses);
    }

    Ok(MemoryScanStats {
        files_seen,
        upserted,
        deleted,
        schema_misses,
    })
}

fn is_md_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

fn stem_fallback(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn load_existing_rows(storage: &Storage) -> Result<HashMap<String, MemoryRegistryRow>> {
    storage.with_transaction(|tx| {
        let mut stmt = tx.prepare(
            "SELECT file_path, project, slug, description, mem_type,
                    origin_session_id, modified_ts, file_mtime, content_hash,
                    links_json, last_seen_scan
             FROM memory_registry",
        )?;
        let mapped = stmt.query_map([], |row| {
            Ok(MemoryRegistryRow {
                file_path: row.get(0)?,
                project: row.get(1)?,
                slug: row.get(2)?,
                description: row.get(3)?,
                mem_type: row.get(4)?,
                origin_session_id: row.get(5)?,
                modified_ts: row.get(6)?,
                file_mtime: row.get(7)?,
                content_hash: row.get(8)?,
                links_json: row.get(9)?,
                last_seen_scan: row.get(10)?,
            })
        })?;
        let mut out = HashMap::new();
        for row in mapped {
            let row = row?;
            out.insert(row.file_path.clone(), row);
        }
        Ok(out)
    })
}

fn build_row_for_file(
    path: &Path,
    file_path: &str,
    project: &str,
    file_mtime: i64,
    size: u64,
    last_seen_scan: i64,
    schema_misses: &mut usize,
) -> MemoryRegistryRow {
    // Over-cap: do not read the file. content_hash = sha256 of empty bytes
    // (documented choice — we never read any of the oversized body).
    if size > READ_CAP_BYTES {
        *schema_misses += 1;
        return MemoryRegistryRow {
            file_path: file_path.to_string(),
            project: project.to_string(),
            slug: stem_fallback(path),
            description: None,
            mem_type: None,
            origin_session_id: None,
            modified_ts: None,
            file_mtime,
            content_hash: hex_sha256(&[]),
            links_json: "[]".to_string(),
            last_seen_scan,
        };
    }

    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(_) => {
            *schema_misses += 1;
            return MemoryRegistryRow {
                file_path: file_path.to_string(),
                project: project.to_string(),
                slug: stem_fallback(path),
                description: None,
                mem_type: None,
                origin_session_id: None,
                modified_ts: None,
                file_mtime,
                content_hash: hex_sha256(&[]),
                links_json: "[]".to_string(),
                last_seen_scan,
            };
        }
    };

    let content_hash = hex_sha256(&bytes);
    let text = String::from_utf8_lossy(&bytes);
    let parsed = parse_frontmatter(&text);
    if parsed.schema_miss {
        *schema_misses += 1;
    }

    let slug = parsed
        .name
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| stem_fallback(path));

    let links = harvest_wikilinks(&text);
    let links_json = serde_json::to_string(&links).unwrap_or_else(|_| "[]".to_string());

    MemoryRegistryRow {
        file_path: file_path.to_string(),
        project: project.to_string(),
        slug,
        description: parsed.description,
        mem_type: parsed.mem_type,
        origin_session_id: parsed.origin_session_id,
        modified_ts: parsed.modified_ts,
        file_mtime,
        content_hash,
        links_json,
        last_seen_scan,
    }
}

#[derive(Debug, Default)]
struct ParsedFrontmatter {
    name: Option<String>,
    description: Option<String>,
    mem_type: Option<String>,
    origin_session_id: Option<String>,
    modified_ts: Option<String>,
    /// True when opening `---` is absent, or closing fence is missing.
    schema_miss: bool,
}

/// Hand-rolled frontmatter parser — no YAML crate.
fn parse_frontmatter(content: &str) -> ParsedFrontmatter {
    let mut lines = content.lines();
    let Some(first) = lines.next() else {
        return ParsedFrontmatter {
            schema_miss: true,
            ..Default::default()
        };
    };
    // Exact `---` after trimming trailing whitespace/\r (lines() already strips
    // line endings; trim_end covers leftover whitespace).
    if first.trim_end() != "---" {
        return ParsedFrontmatter {
            schema_miss: true,
            ..Default::default()
        };
    }

    let mut out = ParsedFrontmatter::default();
    let mut in_metadata = false;
    let mut closed = false;

    for line in lines {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }

        let is_indented = line.starts_with(' ') || line.starts_with('\t');
        if is_indented {
            if in_metadata {
                if let Some((key, value)) = parse_kv_line(line.trim()) {
                    apply_metadata_field(&mut out, &key, value);
                }
            }
            continue;
        }

        // Zero-indentation ends the metadata child block.
        in_metadata = false;
        if let Some((key, value)) = parse_kv_line(line) {
            match key.as_str() {
                "name" if !value.is_empty() => out.name = Some(value),
                "description" => out.description = Some(value),
                "metadata" => in_metadata = true,
                _ => {}
            }
        }
    }

    if !closed {
        // Malformed: discard partial fields; caller falls back to filename slug.
        return ParsedFrontmatter {
            schema_miss: true,
            ..Default::default()
        };
    }

    out
}

fn parse_kv_line(line: &str) -> Option<(String, String)> {
    let (key, rest) = line.split_once(':')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    let value = strip_surrounding_quotes(rest.trim()).to_string();
    Some((key.to_string(), value))
}

fn strip_surrounding_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn apply_metadata_field(out: &mut ParsedFrontmatter, key: &str, value: String) {
    match key {
        "type" => out.mem_type = Some(value),
        "originSessionId" => out.origin_session_id = Some(value),
        "modified" => out.modified_ts = Some(value),
        _ => {}
    }
}

fn harvest_wikilinks(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for cap in WIKILINK_RE.captures_iter(content) {
        let link = cap[1].to_string();
        if !out.contains(&link) {
            out.push(link);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn write_memory(dir: &Path, project: &str, filename: &str, content: &str) -> PathBuf {
        let mem = dir.join(project).join("memory");
        fs::create_dir_all(&mem).unwrap();
        let path = mem.join(filename);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    fn get_row(storage: &Storage, file_path: &str) -> Option<MemoryRegistryRow> {
        storage
            .with_transaction(|tx| {
                let result = tx.query_row(
                    "SELECT file_path, project, slug, description, mem_type,
                            origin_session_id, modified_ts, file_mtime, content_hash,
                            links_json, last_seen_scan
                     FROM memory_registry WHERE file_path = ?1",
                    [file_path],
                    |row| {
                        Ok(MemoryRegistryRow {
                            file_path: row.get(0)?,
                            project: row.get(1)?,
                            slug: row.get(2)?,
                            description: row.get(3)?,
                            mem_type: row.get(4)?,
                            origin_session_id: row.get(5)?,
                            modified_ts: row.get(6)?,
                            file_mtime: row.get(7)?,
                            content_hash: row.get(8)?,
                            links_json: row.get(9)?,
                            last_seen_scan: row.get(10)?,
                        })
                    },
                );
                match result {
                    Ok(row) => Ok(Some(row)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(e.into()),
                }
            })
            .unwrap()
    }

    fn count_project(storage: &Storage, project: &str) -> i64 {
        storage
            .with_transaction(|tx| {
                tx.query_row(
                    "SELECT COUNT(*) FROM memory_registry WHERE project = ?1",
                    [project],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap()
    }

    fn good_frontmatter() -> &'static str {
        r#"---
name: reference-email-sending-domain-dns
description: "Email sending domain email.anukriti.ai — DNS legs"
metadata:
  node_type: memory
  type: reference
  originSessionId: 723f8a5e-341b-4e41-b2eb-dffd02ef440e
  modified: 2026-08-19T15:05:51.330Z
---
Body text with a [[some-slug]] wikilink and more prose.
"#
    }

    #[test]
    fn parses_good_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_memory(dir.path(), "proj1", "reference_x.md", good_frontmatter());
        let storage = Storage::open_memory().unwrap();

        let stats = scan_memory_dirs(&storage, dir.path()).unwrap();
        assert_eq!(stats.files_seen, 1);
        assert_eq!(stats.upserted, 1);
        assert_eq!(stats.schema_misses, 0);
        assert_eq!(stats.deleted, 0);

        let row = get_row(&storage, &path.to_string_lossy()).unwrap();
        assert_eq!(row.slug, "reference-email-sending-domain-dns");
        assert_eq!(
            row.description.as_deref(),
            Some("Email sending domain email.anukriti.ai — DNS legs")
        );
        assert_eq!(row.mem_type.as_deref(), Some("reference"));
        assert_eq!(
            row.origin_session_id.as_deref(),
            Some("723f8a5e-341b-4e41-b2eb-dffd02ef440e")
        );
        assert_eq!(row.modified_ts.as_deref(), Some("2026-08-19T15:05:51.330Z"));
        let links: Vec<String> = serde_json::from_str(&row.links_json).unwrap();
        assert_eq!(links, vec!["some-slug".to_string()]);
        assert_eq!(row.project, "proj1");
    }

    #[test]
    fn missing_frontmatter_is_schema_miss() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_memory(
            dir.path(),
            "proj1",
            "plain_note.md",
            "Just plain text, no fences at all.\n",
        );
        let storage = Storage::open_memory().unwrap();

        let stats = scan_memory_dirs(&storage, dir.path()).unwrap();
        assert_eq!(stats.schema_misses, 1);
        assert_eq!(stats.upserted, 1);
        assert_eq!(stats.files_seen, 1);

        let row = get_row(&storage, &path.to_string_lossy()).unwrap();
        assert_eq!(row.slug, "plain_note");
    }

    #[test]
    fn malformed_frontmatter_no_closing_fence() {
        let dir = tempfile::tempdir().unwrap();
        let content = r#"---
name: would-be-name
description: never closed
metadata:
  type: reference
"#;
        let path = write_memory(dir.path(), "proj1", "broken_fence.md", content);
        let storage = Storage::open_memory().unwrap();

        let stats = scan_memory_dirs(&storage, dir.path()).unwrap();
        assert_eq!(stats.schema_misses, 1);
        assert_eq!(stats.upserted, 1);

        let row = get_row(&storage, &path.to_string_lossy()).unwrap();
        assert_eq!(row.slug, "broken_fence");
    }

    #[test]
    fn quoted_values_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let content = r#"---
name: quoted-desc
description: "some value"
metadata:
  type: reference
---
body
"#;
        let path = write_memory(dir.path(), "proj1", "quoted.md", content);
        let storage = Storage::open_memory().unwrap();

        scan_memory_dirs(&storage, dir.path()).unwrap();
        let row = get_row(&storage, &path.to_string_lossy()).unwrap();
        assert_eq!(row.description.as_deref(), Some("some value"));
    }

    #[test]
    fn metadata_children_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let content = r#"---
name: meta-children
description: demo
unused_field: ignore-me
metadata:
  type: reference
  originSessionId: abc-123
  modified: 2026-01-01T00:00:00.000Z
---
body
"#;
        let path = write_memory(dir.path(), "proj1", "meta_children.md", content);
        let storage = Storage::open_memory().unwrap();

        let stats = scan_memory_dirs(&storage, dir.path()).unwrap();
        assert_eq!(stats.schema_misses, 0);

        let row = get_row(&storage, &path.to_string_lossy()).unwrap();
        assert_eq!(row.mem_type.as_deref(), Some("reference"));
        assert_eq!(row.origin_session_id.as_deref(), Some("abc-123"));
        assert_eq!(row.modified_ts.as_deref(), Some("2026-01-01T00:00:00.000Z"));
        assert_eq!(row.slug, "meta-children");
    }

    #[test]
    fn memory_md_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_memory(dir.path(), "proj1", "MEMORY.md", "# index\n");
        write_memory(dir.path(), "proj1", "reference_real.md", good_frontmatter());
        // Case-insensitive skip in a sibling project.
        write_memory(dir.path(), "proj2", "Memory.md", "# also index\n");
        write_memory(dir.path(), "proj2", "MEMORY.MD", "# ALSO INDEX\n");
        write_memory(
            dir.path(),
            "proj2",
            "real_two.md",
            r#"---
name: real-two
description: ok
metadata:
  type: reference
---
body
"#,
        );

        let storage = Storage::open_memory().unwrap();
        let stats = scan_memory_dirs(&storage, dir.path()).unwrap();
        // Only the two real memory files count.
        assert_eq!(stats.files_seen, 2);
        assert_eq!(count_project(&storage, "proj1"), 1);
        assert_eq!(count_project(&storage, "proj2"), 1);
    }

    #[test]
    fn delete_on_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path_a = write_memory(
            dir.path(),
            "proj1",
            "keep_me.md",
            r#"---
name: keep-me
description: stays
metadata:
  type: reference
---
a
"#,
        );
        let path_b = write_memory(
            dir.path(),
            "proj1",
            "drop_me.md",
            r#"---
name: drop-me
description: goes
metadata:
  type: reference
---
b
"#,
        );
        let storage = Storage::open_memory().unwrap();

        let first = scan_memory_dirs(&storage, dir.path()).unwrap();
        assert_eq!(first.files_seen, 2);
        assert_eq!(first.upserted, 2);
        assert_eq!(count_project(&storage, "proj1"), 2);

        fs::remove_file(&path_b).unwrap();
        let _ = path_a; // keep path_a on disk

        let second = scan_memory_dirs(&storage, dir.path()).unwrap();
        assert_eq!(second.files_seen, 1);
        assert_eq!(second.deleted, 1);
        assert_eq!(count_project(&storage, "proj1"), 1);
    }

    #[test]
    fn mtime_skip_does_not_delete_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_memory(dir.path(), "proj1", "stable.md", good_frontmatter());
        let storage = Storage::open_memory().unwrap();

        let first = scan_memory_dirs(&storage, dir.path()).unwrap();
        assert_eq!(first.upserted, 1);
        let before = get_row(&storage, &path.to_string_lossy()).unwrap();

        let second = scan_memory_dirs(&storage, dir.path()).unwrap();
        assert_eq!(second.files_seen, 1);
        assert_eq!(second.upserted, 0);
        assert_eq!(second.deleted, 0);

        let after = get_row(&storage, &path.to_string_lossy()).unwrap();
        assert_eq!(after.slug, before.slug);
        assert_eq!(after.description, before.description);
        assert_eq!(after.mem_type, before.mem_type);
        assert_eq!(after.origin_session_id, before.origin_session_id);
        assert_eq!(after.modified_ts, before.modified_ts);
        assert_eq!(after.content_hash, before.content_hash);
        assert_eq!(after.links_json, before.links_json);
        assert_eq!(after.file_mtime, before.file_mtime);
        // Generation advanced, but content fields intact.
        assert!(after.last_seen_scan > before.last_seen_scan);
    }

    #[test]
    fn over_cap_file_is_metadata_only_schema_miss() {
        let dir = tempfile::tempdir().unwrap();
        let mem = dir.path().join("proj1").join("memory");
        fs::create_dir_all(&mem).unwrap();
        let path = mem.join("huge_file.md");
        let mut f = fs::File::create(&path).unwrap();
        // 262145 bytes — one over the cap.
        let chunk = vec![b'x'; 4096];
        let mut written = 0u64;
        while written < READ_CAP_BYTES + 1 {
            let n = ((READ_CAP_BYTES + 1) - written).min(chunk.len() as u64) as usize;
            f.write_all(&chunk[..n]).unwrap();
            written += n as u64;
        }
        drop(f);

        let storage = Storage::open_memory().unwrap();
        let stats = scan_memory_dirs(&storage, dir.path()).unwrap();
        assert_eq!(stats.schema_misses, 1);
        assert_eq!(stats.upserted, 1);

        let row = get_row(&storage, &path.to_string_lossy()).unwrap();
        assert_eq!(row.slug, "huge_file");
        assert!(row.description.is_none());
        assert!(row.mem_type.is_none());
        assert!(row.origin_session_id.is_none());
        assert_eq!(row.links_json, "[]");
    }

    #[test]
    fn sibling_without_memory_dir_does_not_affect_other() {
        let dir = tempfile::tempdir().unwrap();
        write_memory(dir.path(), "proj1", "only.md", good_frontmatter());
        // proj2 exists but has no memory/ subdirectory.
        fs::create_dir_all(dir.path().join("proj2")).unwrap();

        let storage = Storage::open_memory().unwrap();
        let stats = scan_memory_dirs(&storage, dir.path()).unwrap();
        assert_eq!(stats.files_seen, 1);
        assert_eq!(stats.upserted, 1);
        assert_eq!(count_project(&storage, "proj1"), 1);
        assert_eq!(count_project(&storage, "proj2"), 0);

        // Second scan still leaves proj1 intact.
        let stats2 = scan_memory_dirs(&storage, dir.path()).unwrap();
        assert_eq!(stats2.deleted, 0);
        assert_eq!(count_project(&storage, "proj1"), 1);
    }

    #[test]
    fn missing_projects_root_returns_default() {
        let storage = Storage::open_memory().unwrap();
        let missing = PathBuf::from("/tmp/csr-memory-registry-does-not-exist-xyz");
        let stats = scan_memory_dirs(&storage, &missing).unwrap();
        assert_eq!(stats, MemoryScanStats::default());
    }
}
