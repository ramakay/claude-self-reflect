//! Read-only field measurement. One stretch starts at a main-thread compact
//! boundary and ends at the next boundary or EOF. Calls before it do not count.
use super::{ParsedTranscript, Role};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Serialize)]
pub struct Reach {
    pub post_compact_stretches: usize,
    pub stretches_with_csr_transcript: usize,
    pub stretches_with_receipt: usize,
    pub receipt_stretches_with_csr_transcript: usize,
    pub sessions_read: usize,
    pub files_failed: usize,
    pub unrecognized_entries: usize,
}

impl Reach {
    pub fn text(&self) -> String {
        format!("csr_transcript reach: {}/{} post-compact stretches\nReceipt observed in transcript: {}/{} stretches used csr_transcript\nSessions read: {}; failed files: {}; unrecognized entries: {}\nOne stretch = compact boundary through next boundary or EOF; repeated calls count once.\n", self.stretches_with_csr_transcript, self.post_compact_stretches, self.receipt_stretches_with_csr_transcript, self.stretches_with_receipt, self.sessions_read, self.files_failed, self.unrecognized_entries)
    }
}

pub fn count(
    parsed: &ParsedTranscript,
    since: Option<chrono::DateTime<chrono::FixedOffset>>,
) -> Reach {
    let mut out = Reach {
        sessions_read: 1,
        unrecognized_entries: parsed.unrecognized_entries,
        ..Reach::default()
    };
    let mut active = false;
    let mut used = false;
    let mut receipt = false;
    let mut seen = HashSet::new();
    let finish = |out: &mut Reach, active: bool, used: bool, receipt: bool| {
        if active {
            out.post_compact_stretches += 1;
            out.stretches_with_csr_transcript += usize::from(used);
            out.stretches_with_receipt += usize::from(receipt);
            out.receipt_stretches_with_csr_transcript += usize::from(receipt && used);
        }
    };
    let mut hooks = parsed.hook_receipts.iter().peekable();
    // A hook attachment holds no turn. Its place is the count of entries before it.
    let mut see_hooks = |upto: usize, active: bool, receipt: &mut bool| {
        while hooks.next_if(|at| **at <= upto).is_some() {
            *receipt |= active;
        }
    };
    for (index, e) in parsed.entries.iter().enumerate() {
        see_hooks(index, active, &mut receipt);
        if e.is_sidechain {
            continue;
        }
        if e.is_compact_boundary() {
            if e.uuid.as_ref().is_some_and(|id| !seen.insert(id.clone())) {
                continue;
            }
            finish(&mut out, active, used, receipt);
            active = since.is_none_or(|cut| {
                e.timestamp
                    .as_deref()
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .is_some_and(|t| t >= cut)
            });
            used = false;
            receipt = false;
        } else if active {
            // Count actual tool invocations, never a mention in a message or result.
            used |= e.role == Role::Assistant
                && e.tool_uses
                    .iter()
                    .any(|t| t.name == "csr_transcript" || t.name.ends_with("__csr_transcript"));
            receipt |= (e.is_compact_summary || e.role == Role::System)
                && e.text.contains(super::receipt::SENTINEL);
        }
    }
    see_hooks(parsed.entries.len(), active, &mut receipt);
    finish(&mut out, active, used, receipt);
    out
}

fn collect(
    root: &Path,
    dir: &Path,
    mirrored: bool,
    files: &mut BTreeMap<PathBuf, PathBuf>,
) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for item in std::fs::read_dir(dir)? {
        let item = item?;
        let kind = item.file_type()?;
        let path = item.path();
        // Only main-thread sessions. No symlinks and no subagent directories.
        if kind.is_dir() && item.file_name() != "subagents" {
            collect(root, &path, mirrored, files)?;
        } else if kind.is_file() {
            let name = item.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("agent-") {
                continue;
            }
            if (mirrored && name.ends_with(".jsonl.zst")) || (!mirrored && name.ends_with(".jsonl"))
            {
                let mut key = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                if mirrored {
                    key.set_extension("");
                }
                files.insert(key, path);
            }
        }
    }
    Ok(())
}

/// Mirror first, live second: the same relative session path counts once.
/// No DB, embeddings, telemetry writes, or network.
pub fn scan(projects: &Path, data: &Path, since: Option<&str>) -> anyhow::Result<Reach> {
    let since = since
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()?;
    let mut files = BTreeMap::new();
    let mirror = crate::mirror::transcripts_dir(data);
    collect(&mirror, &mirror, true, &mut files)?;
    collect(projects, projects, false, &mut files)?;
    let mut total = Reach::default();
    for path in files.values() {
        let parsed = if path.extension().is_some_and(|e| e == "zst") {
            super::parse_mirrored_transcript(path)
        } else {
            super::parse_transcript(path)
        };
        let Ok(parsed) = parsed else {
            total.files_failed += 1;
            continue;
        };
        if parsed.read_error.is_some() {
            total.files_failed += 1;
            continue;
        }
        let row = count(&parsed, since);
        total.post_compact_stretches += row.post_compact_stretches;
        total.stretches_with_csr_transcript += row.stretches_with_csr_transcript;
        total.stretches_with_receipt += row.stretches_with_receipt;
        total.receipt_stretches_with_csr_transcript += row.receipt_stretches_with_csr_transcript;
        total.sessions_read += 1;
        total.unrecognized_entries += row.unrecognized_entries;
    }
    Ok(total)
}
