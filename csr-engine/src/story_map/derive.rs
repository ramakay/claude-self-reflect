//! Receipts-only `@shifts` derivation for extracted compaction stories.
//!
//! Synthesizes up to 4 [`ShiftEntry`] items directly from a conversation's
//! `code_evolution` rows without any model call. Fails open on any query or
//! parse error by returning an empty list.
//!
//! Besides the per-file "changed / added N symbols" entries, a single edit that
//! removed and added names of the same kind (functions, types, imports) becomes
//! a before -> after entry, so a receipts-only map has a real before side.
//! Ranking: import and type swaps, non-test function swaps, per-file entries,
//! test-only swaps (names starting `test_`, or files under `tests/`).

use rusqlite::{params, Connection};

use super::{parse_shift_line, story_map_disabled, ShiftEntry};
use crate::storage::Storage;

const MAX_EVO_ROWS: usize = 2000;
const MAX_ENTRIES: usize = 4;
const MAX_SCOPE_CHARS: usize = 48;
const MAX_SYM_CHARS: usize = 40;
const MAX_CLAIM_CHARS: usize = 72;
const MAX_IMPORT_SWAPS: usize = 2;

#[derive(Debug, Clone)]
struct SwapPair {
    added: Vec<String>,
    removed: Vec<String>,
}

#[derive(Debug, Default)]
struct FileAccum {
    file_path: String,
    language: String,
    added: Vec<String>,
    removed: Vec<String>,
    touches: usize,
    func_swaps: Vec<SwapPair>,
    type_swaps: Vec<SwapPair>,
    import_swaps: Vec<SwapPair>,
}

#[derive(Debug)]
struct RankedCandidate {
    tier: u8,
    /// Import swaps beyond `MAX_IMPORT_SWAPS` drop behind the function swaps.
    is_import: bool,
    demoted: bool,
    has_symbol_change: bool,
    distinct_symbols: usize,
    touches: usize,
    scope: String,
    entry: ShiftEntry,
}

#[derive(Debug, Clone)]
struct ParsedImport {
    module: String,
    items: Vec<String>,
}

/// Derives up to 4 [`ShiftEntry`] items from this conversation's `code_evolution`
/// rows alone (zero model calls).
///
/// Returns an empty `Vec` when `story_map_disabled()`, `conv_id` is empty, or on
/// any SQLite error.
pub fn derive_receipt_shifts(storage: &Storage, conv_id: &str) -> Vec<ShiftEntry> {
    if story_map_disabled() || conv_id.is_empty() {
        return Vec::new();
    }
    storage
        .with_connection(|conn| Ok(derive_with_conn(conn, conv_id)))
        .unwrap_or_default()
}

fn derive_with_conn(conn: &Connection, conv_id: &str) -> Vec<ShiftEntry> {
    let sql = "SELECT id, file_path, language,
                      functions_added, functions_removed,
                      types_added, types_removed,
                      imports_added, imports_removed
               FROM code_evolution
               WHERE session_id = ?1
               ORDER BY timestamp
               LIMIT ?2";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![conv_id, MAX_EVO_ROWS as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            r.get::<_, Option<String>>(7)?.unwrap_or_default(),
            r.get::<_, Option<String>>(8)?.unwrap_or_default(),
        ))
    }) else {
        return Vec::new();
    };

    let mut files: Vec<FileAccum> = Vec::new();
    for row in rows.flatten() {
        let (_id, file_path, language, fa, fr, ta, tr, ia, ir) = row;
        if file_path.is_empty() {
            continue;
        }
        let acc = match files.iter_mut().find(|f| f.file_path == file_path) {
            Some(existing) => existing,
            None => {
                files.push(FileAccum {
                    file_path,
                    language: language.clone(),
                    ..Default::default()
                });
                files.last_mut().expect("just pushed")
            }
        };
        if (acc.language.is_empty() || acc.language == "text") && !language.is_empty() {
            acc.language = language.clone();
        }
        acc.touches += 1;

        if language != "text" {
            let fa_list = parse_valid_syms(&fa);
            let fr_list = parse_valid_syms(&fr);
            let ta_list = parse_valid_syms(&ta);
            let tr_list = parse_valid_syms(&tr);

            for sym in fa_list.iter().chain(ta_list.iter()) {
                if !acc.added.contains(sym) {
                    acc.added.push(sym.clone());
                }
            }
            for sym in fr_list.iter().chain(tr_list.iter()) {
                if !acc.removed.contains(sym) {
                    acc.removed.push(sym.clone());
                }
            }

            acc.func_swaps
                .extend(extract_row_sym_swap(&fa_list, &fr_list));
            acc.type_swaps
                .extend(extract_row_sym_swap(&ta_list, &tr_list));
            acc.import_swaps.extend(extract_row_import_swap(&ia, &ir));
        }
    }

    let mut candidates: Vec<RankedCandidate> = Vec::with_capacity(files.len() * 2);
    for file in files {
        let Some((scope, basename)) = extract_scope_and_basename(&file.file_path) else {
            continue;
        };
        let in_tests_dir = has_tests_dir_component(&file.file_path);

        let sym_swap = |swaps: &[SwapPair], is_type: bool| {
            best_candidate(swaps.iter().filter_map(|swap| {
                build_sym_swap_candidate(
                    &scope,
                    &basename,
                    file.touches,
                    in_tests_dir,
                    is_type,
                    swap,
                    &file.added,
                    &file.removed,
                )
            }))
        };
        let import_swap = best_candidate(file.import_swaps.iter().filter_map(|swap| {
            build_import_swap_candidate(&scope, file.touches, in_tests_dir, swap)
        }));
        candidates.extend(import_swap);
        candidates.extend(sym_swap(&file.type_swaps, true));
        candidates.extend(sym_swap(&file.func_swaps, false));

        let mut changed: Vec<String> = Vec::new();
        let mut added_only: Vec<String> = Vec::new();
        for sym in &file.added {
            if file.removed.contains(sym) {
                changed.push(sym.clone());
            } else {
                added_only.push(sym.clone());
            }
        }
        let removed_only: Vec<String> = file
            .removed
            .into_iter()
            .filter(|sym| !file.added.contains(sym))
            .collect();

        let distinct_symbols = changed.len() + added_only.len() + removed_only.len();
        let has_symbol_change = file.language != "text" && distinct_symbols > 0;

        let (before, after, raw_claim) = if distinct_symbols >= 2 {
            (
                "-".to_string(),
                format!("{scope}~"),
                format_multi_symbol_claim(
                    changed.len(),
                    added_only.len(),
                    removed_only.len(),
                    &basename,
                ),
            )
        } else if let Some(sym) = changed.first() {
            (
                "-".to_string(),
                format!("{scope}:{sym}~"),
                format!("Changed {sym}"),
            )
        } else if let Some(sym) = added_only.first() {
            (
                "-".to_string(),
                format!("{scope}:{sym}+"),
                format!("Added {sym}"),
            )
        } else if let Some(sym) = removed_only.first() {
            (
                format!("{scope}:{sym}-"),
                "-".to_string(),
                format!("Removed {sym}"),
            )
        } else {
            (
                "-".to_string(),
                format!("{scope}~"),
                format!("Edited {basename}"),
            )
        };

        let claim = sanitize_claim(&raw_claim);
        let line = format!("shift|{before}|{after}|{claim}");
        let Some(entry) = parse_shift_line(&line) else {
            continue;
        };

        candidates.push(RankedCandidate {
            tier: 2,
            is_import: false,
            demoted: false,
            has_symbol_change,
            distinct_symbols,
            touches: file.touches,
            scope,
            entry,
        });
    }

    candidates.sort_by(rank_order);
    // A few import tweaks read as architecture; a fourth `use` line must not crowd out a real
    // function swap, so imports past the cap fall in behind the function swaps.
    let mut imports_kept = 0;
    for cand in candidates.iter_mut().filter(|c| c.is_import && c.tier == 0) {
        imports_kept += 1;
        if imports_kept > MAX_IMPORT_SWAPS {
            cand.tier = 1;
            cand.demoted = true;
        }
    }
    candidates.sort_by(rank_order);

    candidates
        .into_iter()
        .take(MAX_ENTRIES)
        .map(|c| c.entry)
        .collect()
}

fn rank_order(a: &RankedCandidate, b: &RankedCandidate) -> std::cmp::Ordering {
    a.tier
        .cmp(&b.tier)
        .then_with(|| a.demoted.cmp(&b.demoted))
        .then_with(|| b.has_symbol_change.cmp(&a.has_symbol_change))
        .then_with(|| b.distinct_symbols.cmp(&a.distinct_symbols))
        .then_with(|| b.touches.cmp(&a.touches))
        .then_with(|| a.scope.cmp(&b.scope))
}

fn parse_json_str_list(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn parse_valid_syms(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for sym in parse_json_str_list(raw) {
        if is_valid_sym(&sym) && !out.contains(&sym) {
            out.push(sym);
        }
    }
    out
}

/// The swap candidate that names the most symbols; the earliest edit wins a tie.
fn best_candidate(cands: impl Iterator<Item = RankedCandidate>) -> Option<RankedCandidate> {
    let mut best: Option<RankedCandidate> = None;
    for cand in cands {
        if best
            .as_ref()
            .is_none_or(|b| cand.distinct_symbols > b.distinct_symbols)
        {
            best = Some(cand);
        }
    }
    best
}

fn extract_row_sym_swap(added: &[String], removed: &[String]) -> Option<SwapPair> {
    let a_only: Vec<String> = added
        .iter()
        .filter(|s| !removed.contains(s))
        .cloned()
        .collect();
    let r_only: Vec<String> = removed
        .iter()
        .filter(|s| !added.contains(s))
        .cloned()
        .collect();
    if a_only.is_empty() || r_only.is_empty() {
        return None;
    }
    Some(SwapPair {
        added: a_only,
        removed: r_only,
    })
}

fn normalize_import_stmt(s: &str) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.trim_end_matches(';').trim().to_string()
}

fn extract_row_import_swap(ia_raw: &str, ir_raw: &str) -> Option<SwapPair> {
    let mut ia = Vec::new();
    for s in parse_json_str_list(ia_raw) {
        let norm = normalize_import_stmt(&s);
        if !norm.is_empty() && !ia.contains(&norm) {
            ia.push(norm);
        }
    }
    let mut ir = Vec::new();
    for s in parse_json_str_list(ir_raw) {
        let norm = normalize_import_stmt(&s);
        if !norm.is_empty() && !ir.contains(&norm) {
            ir.push(norm);
        }
    }
    let a_only: Vec<String> = ia.iter().filter(|s| !ir.contains(s)).cloned().collect();
    let r_only: Vec<String> = ir.iter().filter(|s| !ia.contains(s)).cloned().collect();
    if a_only.is_empty() || r_only.is_empty() {
        return None;
    }
    Some(SwapPair {
        added: a_only,
        removed: r_only,
    })
}

fn has_tests_dir_component(file_path: &str) -> bool {
    let parts: Vec<&str> = file_path.split('/').filter(|p| !p.is_empty()).collect();
    parts.len() >= 2 && parts[..parts.len() - 1].contains(&"tests")
}

#[allow(clippy::too_many_arguments)]
fn build_sym_swap_candidate(
    scope: &str,
    basename: &str,
    touches: usize,
    in_tests_dir: bool,
    is_type: bool,
    swap: &SwapPair,
    file_added: &[String],
    file_removed: &[String],
) -> Option<RankedCandidate> {
    let added: Vec<String> = swap
        .added
        .iter()
        .filter(|s| !file_removed.contains(s))
        .cloned()
        .collect();
    let removed: Vec<String> = swap
        .removed
        .iter()
        .filter(|s| !file_added.contains(s))
        .cloned()
        .collect();
    if added.is_empty() || removed.is_empty() {
        return None;
    }

    let (r, a) = pick_best_sym_pair(&removed, &added)?;
    let raw_claim = format_sym_swap_claim(r, removed.len(), a, added.len(), basename);
    let claim = sanitize_claim(&raw_claim);
    let before = format!("{scope}:{r}-");
    let after = format!("{scope}:{a}+");
    let line = format!("shift|{before}|{after}|{claim}");
    let entry = parse_shift_line(&line)?;

    let all_test_names = removed
        .iter()
        .chain(added.iter())
        .all(|s| s.starts_with("test_"));
    let tier = if in_tests_dir || all_test_names {
        3
    } else if is_type {
        0
    } else {
        1
    };

    Some(RankedCandidate {
        tier,
        is_import: false,
        demoted: false,
        has_symbol_change: true,
        distinct_symbols: removed.len() + added.len(),
        touches,
        scope: scope.to_string(),
        entry,
    })
}

fn pick_best_sym_pair<'a>(
    removed: &'a [String],
    added: &'a [String],
) -> Option<(&'a str, &'a str)> {
    let mut best: Option<(&'a str, &'a str, usize, usize)> = None;
    for r in removed {
        for a in added {
            let shared = shared_underscore_words(r, a);
            let len_diff = r.chars().count().abs_diff(a.chars().count());
            let better = match best {
                None => true,
                Some((_, _, best_shared, best_diff)) => {
                    shared > best_shared || (shared == best_shared && len_diff < best_diff)
                }
            };
            if better {
                best = Some((r.as_str(), a.as_str(), shared, len_diff));
            }
        }
    }
    best.map(|(r, a, _, _)| (r, a))
}

fn shared_underscore_words(r: &str, a: &str) -> usize {
    let r_words: Vec<&str> = r.split('_').filter(|w| !w.is_empty()).collect();
    let a_words: Vec<&str> = a.split('_').filter(|w| !w.is_empty()).collect();
    let mut unique_r: Vec<&str> = Vec::new();
    for w in r_words {
        if !unique_r.contains(&w) {
            unique_r.push(w);
        }
    }
    unique_r.into_iter().filter(|w| a_words.contains(w)).count()
}

fn count_symbols(n: usize) -> String {
    if n == 1 {
        "1 symbol".to_string()
    } else {
        format!("{n} symbols")
    }
}

/// `Replaced r[ and N more] with a[ and M more] in file`, shortened step by step to fit the
/// 72-char claim cap without cutting a word: drop the file, then name only the removed side,
/// then fall back to counts.
fn format_sym_swap_claim(r: &str, r_len: usize, a: &str, a_len: usize, basename: &str) -> String {
    let more = |n: usize| {
        if n > 1 {
            format!(" and {} more", n - 1)
        } else {
            String::new()
        }
    };
    let (r_more, a_more) = (more(r_len), more(a_len));
    let mut tries = vec![
        format!("Replaced {r}{r_more} with {a}{a_more} in {basename}"),
        format!("Replaced {r}{r_more} with {a}{a_more}"),
    ];
    if a_len > 1 {
        let n = count_symbols(a_len);
        tries.push(format!("Replaced {r}{r_more} with {n} in {basename}"));
        tries.push(format!("Replaced {r}{r_more} with {n}"));
    }
    tries.push(format!(
        "Replaced {} with {} in {basename}",
        count_symbols(r_len),
        count_symbols(a_len)
    ));
    let last = tries.len() - 1;
    tries
        .into_iter()
        .enumerate()
        .find(|(i, t)| *i == last || t.chars().count() <= MAX_CLAIM_CHARS)
        .map(|(_, t)| t)
        .unwrap_or_default()
}

fn build_import_swap_candidate(
    scope: &str,
    touches: usize,
    in_tests_dir: bool,
    swap: &SwapPair,
) -> Option<RankedCandidate> {
    let r_parsed: Vec<ParsedImport> = swap
        .removed
        .iter()
        .filter_map(|s| parse_import_statement(s))
        .collect();
    let a_parsed: Vec<ParsedImport> = swap
        .added
        .iter()
        .filter_map(|s| parse_import_statement(s))
        .collect();
    if r_parsed.is_empty() || a_parsed.is_empty() {
        return None;
    }

    let mut same_mod: Option<(&ParsedImport, Vec<String>, Vec<String>)> = None;
    for rp in &r_parsed {
        if let Some(ap) = a_parsed.iter().find(|ap| ap.module == rp.module) {
            let added_items: Vec<String> = ap
                .items
                .iter()
                .filter(|i| !rp.items.contains(i))
                .cloned()
                .collect();
            let dropped_items: Vec<String> = rp
                .items
                .iter()
                .filter(|i| !ap.items.contains(i))
                .cloned()
                .collect();
            if !added_items.is_empty() || !dropped_items.is_empty() {
                same_mod = Some((rp, added_items, dropped_items));
                break;
            }
        }
    }

    let (before, after, raw_claim) = if let Some((rp, added_items, dropped_items)) = same_mod {
        let m_disp = shorten_module_display(&rp.module);
        let mod_sym = sanitize_endpoint_sym(&rp.module)?;
        let claim = same_module_claim(&m_disp, &added_items, &dropped_items);
        let (b_ep, a_ep) = if let Some(first_added) = added_items.first() {
            let item_sym = sanitize_endpoint_sym(first_added)?;
            (
                format!("{scope}:{mod_sym}~"),
                format!("{scope}:{item_sym}+"),
            )
        } else {
            let item_sym = sanitize_endpoint_sym(dropped_items.first()?)?;
            (
                format!("{scope}:{item_sym}-"),
                format!("{scope}:{mod_sym}~"),
            )
        };
        (b_ep, a_ep, claim)
    } else {
        let rp = r_parsed
            .iter()
            .find(|rp| !a_parsed.iter().any(|ap| ap.module == rp.module))?;
        let ap = a_parsed
            .iter()
            .find(|ap| !r_parsed.iter().any(|rp2| rp2.module == ap.module))?;
        let mr_disp = shorten_module_display(&rp.module);
        let ma_disp = shorten_module_display(&ap.module);
        let mr_sym = sanitize_endpoint_sym(&rp.module)?;
        let ma_sym = sanitize_endpoint_sym(&ap.module)?;
        let claim = format!("Swapped the {mr_disp} import for {ma_disp}");
        (
            format!("{scope}:{mr_sym}-"),
            format!("{scope}:{ma_sym}+"),
            claim,
        )
    };

    let claim = sanitize_claim(&raw_claim);
    let line = format!("shift|{before}|{after}|{claim}");
    let entry = parse_shift_line(&line)?;

    Some(RankedCandidate {
        tier: if in_tests_dir { 3 } else { 0 },
        is_import: true,
        demoted: false,
        has_symbol_change: true,
        distinct_symbols: swap.removed.len() + swap.added.len(),
        touches,
        scope: scope.to_string(),
        entry,
    })
}

/// `Swapped the m import for one that adds A and drops D`, shortened by counting names when the
/// list does not fit the claim cap (never cut mid-word).
fn same_module_claim(m: &str, added: &[String], dropped: &[String]) -> String {
    let head = format!("Swapped the {m} import for one that");
    let names = |n: usize| if n == 1 { "name" } else { "names" };
    let phrase = |a: String, d: String| match (added.is_empty(), dropped.is_empty()) {
        (false, true) => format!("adds {a}"),
        (true, false) => format!("drops {d}"),
        _ => format!("adds {a} and drops {d}"),
    };
    let tries = [
        phrase(format_item_list(added), format_item_list(dropped)),
        phrase(
            format!("{} {}", added.len(), names(added.len())),
            format!("{} {}", dropped.len(), names(dropped.len())),
        ),
    ];
    tries
        .iter()
        .map(|t| format!("{head} {t}"))
        .find(|c| c.chars().count() <= MAX_CLAIM_CHARS)
        .unwrap_or_else(|| format!("Changed the {m} import"))
}

fn format_item_list(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        2 => format!("{} and {}", items[0], items[1]),
        n => format!("{} and {} more", items[0], n - 1),
    }
}

fn shorten_module_display(m: &str) -> String {
    if m.chars().count() <= 24 {
        return m.to_string();
    }
    let sep = if m.contains("::") {
        "::"
    } else if m.contains('/') {
        "/"
    } else {
        "."
    };
    let parts: Vec<&str> = m.split(sep).filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        let last_two = format!("{}{sep}{}", parts[parts.len() - 2], parts[parts.len() - 1]);
        if last_two.chars().count() <= 24 {
            return last_two;
        }
    }
    parts
        .last()
        .copied()
        .unwrap_or(m)
        .chars()
        .take(24)
        .collect()
}

fn sanitize_endpoint_sym(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_dot = false;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.') {
            if c == '.' {
                if !prev_dot {
                    out.push('.');
                    prev_dot = true;
                }
            } else {
                out.push(c);
                prev_dot = false;
            }
        } else if !prev_dot {
            out.push('.');
            prev_dot = true;
        }
    }
    let trimmed = out.trim_matches('.');
    if trimmed.is_empty() {
        return None;
    }
    let capped: String = trimmed.chars().take(MAX_SYM_CHARS).collect();
    let final_sym = capped.trim_matches('.').to_string();
    if is_valid_sym(&final_sym) {
        Some(final_sym)
    } else {
        None
    }
}

fn parse_import_statement(stmt: &str) -> Option<ParsedImport> {
    let s = normalize_import_stmt(stmt);
    let s = s
        .strip_prefix("pub(crate) ")
        .or_else(|| s.strip_prefix("pub(super) "))
        .or_else(|| s.strip_prefix("pub "))
        .unwrap_or(&s)
        .trim();

    if let Some(rest) = s.strip_prefix("use ") {
        return parse_rust_use(rest.trim());
    }
    if let Some(rest) = s.strip_prefix("import ") {
        return parse_js_or_py_import(rest.trim());
    }
    if let Some(rest) = s.strip_prefix("from ") {
        return parse_py_from_import(rest.trim());
    }
    None
}

fn clean_import_item(item: &str) -> Option<String> {
    let base = item
        .split(" as ")
        .next()
        .unwrap_or(item)
        .trim()
        .trim_matches(|c: char| matches!(c, '{' | '}' | '(' | ')'));
    if base.is_empty() || base == "self" {
        None
    } else {
        Some(base.to_string())
    }
}

fn parse_rust_use(rest: &str) -> Option<ParsedImport> {
    if let Some(brace_idx) = rest.find('{') {
        let prefix = rest[..brace_idx].trim().trim_end_matches("::");
        let close_idx = rest.rfind('}')?;
        if close_idx < brace_idx {
            return None;
        }
        let inner = &rest[brace_idx + 1..close_idx];
        let items: Vec<String> = split_top_level(inner)
            .into_iter()
            .filter_map(clean_import_item)
            .collect();
        if prefix.is_empty() {
            return None;
        }
        return Some(ParsedImport {
            module: prefix.to_string(),
            items,
        });
    }
    if let Some((module, item)) = rest.rsplit_once("::") {
        let module = module.trim();
        let items: Vec<String> = clean_import_item(item).into_iter().collect();
        if module.is_empty() {
            return None;
        }
        return Some(ParsedImport {
            module: module.to_string(),
            items,
        });
    }
    let module = clean_import_item(rest)?;
    Some(ParsedImport {
        module: module.clone(),
        items: vec![module],
    })
}

/// Splits on commas that are not inside `{...}`.
fn split_top_level(s: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0usize, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn strip_quotes(s: &str) -> &str {
    s.trim()
        .trim_matches(|c: char| matches!(c, '\'' | '"' | '`'))
}

fn parse_js_or_py_import(rest: &str) -> Option<ParsedImport> {
    let rest = rest.strip_prefix("type ").unwrap_or(rest).trim();
    if let Some((clauses, mod_part)) = rest.rsplit_once(" from ") {
        let module = strip_quotes(mod_part);
        if module.is_empty() {
            return None;
        }
        let mut items = Vec::new();
        let clauses = clauses.trim();
        if let Some(brace_start) = clauses.find('{') {
            let before_brace = clauses[..brace_start].trim().trim_end_matches(',').trim();
            if let Some(def_item) = clean_import_item(before_brace) {
                items.push(def_item);
            }
            let brace_end = clauses.rfind('}')?;
            if brace_end > brace_start {
                for part in clauses[brace_start + 1..brace_end].split(',') {
                    if let Some(it) = clean_import_item(part) {
                        items.push(it);
                    }
                }
            }
        } else {
            for part in clauses.split(',') {
                let part = part.trim();
                let part = part.strip_prefix("* as ").unwrap_or(part);
                if let Some(it) = clean_import_item(part) {
                    items.push(it);
                }
            }
        }
        return Some(ParsedImport {
            module: module.to_string(),
            items,
        });
    }
    let unquoted = strip_quotes(rest);
    if unquoted.is_empty() {
        return None;
    }
    let first_mod = unquoted.split(',').next()?.trim();
    let mod_name = clean_import_item(first_mod)?;
    Some(ParsedImport {
        module: mod_name.clone(),
        items: vec![mod_name],
    })
}

fn parse_py_from_import(rest: &str) -> Option<ParsedImport> {
    let (mod_part, items_part) = rest.split_once(" import ")?;
    let module = mod_part.trim();
    if module.is_empty() {
        return None;
    }
    let items_clean = items_part
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')');
    let items: Vec<String> = items_clean
        .split(',')
        .filter_map(clean_import_item)
        .collect();
    Some(ParsedImport {
        module: module.to_string(),
        items,
    })
}

fn is_scope_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '@' | '-')
}

fn is_valid_sym(s: &str) -> bool {
    let len = s.chars().count();
    (1..=MAX_SYM_CHARS).contains(&len)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.'))
}

fn extract_scope_and_basename(file_path: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = file_path.split('/').filter(|p| !p.is_empty()).collect();
    let basename = (*parts.last()?).to_string();
    let base_len = basename.chars().count();
    if !(1..=MAX_SCOPE_CHARS).contains(&base_len) || !basename.chars().all(is_scope_char) {
        return None;
    }

    let two_comp = if parts.len() >= 2 {
        format!("{}/{}", parts[parts.len() - 2], basename)
    } else {
        basename.clone()
    };

    let scope = if two_comp.chars().count() <= MAX_SCOPE_CHARS {
        two_comp
    } else {
        basename.clone()
    };

    if !scope.chars().all(is_scope_char) {
        return None;
    }
    Some((scope, basename))
}

fn format_multi_symbol_claim(
    changed: usize,
    added: usize,
    removed: usize,
    basename: &str,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (cap, low, count) in [
        ("Changed", "changed", changed),
        ("Added", "added", added),
        ("Removed", "removed", removed),
    ] {
        if count > 0 {
            let word = if parts.is_empty() { cap } else { low };
            parts.push(format!("{word} {count}"));
        }
    }
    format!("{} symbols in {basename}", parts.join(", "))
}

fn sanitize_claim(raw: &str) -> String {
    let filtered: String = raw
        .chars()
        .filter(|&c| c != '|' && (' '..='~').contains(&c))
        .take(MAX_CLAIM_CHARS)
        .collect();
    filtered.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::story_map::ground::ground_shifts;
    use crate::story_map::store::{build_story_map, load_story_map, save_story_map};
    use crate::story_map::{ShiftEntry, ShiftKind};

    #[allow(clippy::too_many_arguments)]
    fn seed_evo_full(
        storage: &Storage,
        id: &str,
        session_id: &str,
        file_path: &str,
        language: &str,
        ts: &str,
        fa: &str,
        fr: &str,
        ta: &str,
        tr: &str,
    ) {
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO code_evolution (
                        id, session_id, project_name, file_path, language, timestamp,
                        functions_added, functions_removed, types_added, types_removed
                    ) VALUES (?1, ?2, 'csr', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![id, session_id, file_path, language, ts, fa, fr, ta, tr],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn added_symbol_makes_an_after_only_entry() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_full(
            &storage,
            "evo_10000001_1",
            "conv_1",
            "/repo/csr-engine/src/summarizer.rs",
            "rust",
            "2026-10-14T00:00:01Z",
            r#"["ground_shifts"]"#,
            "[]",
            "[]",
            "[]",
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, ShiftKind::Shift);
        assert!(entries[0].before.is_none());
        assert_eq!(
            ShiftEntry::render_side(&entries[0].after),
            "src/summarizer.rs:ground_shifts+"
        );
        assert_eq!(entries[0].claim, "Added ground_shifts");
    }

    #[test]
    fn removed_symbol_makes_a_before_only_entry() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_full(
            &storage,
            "evo_20000002_1",
            "conv_1",
            "/repo/csr-engine/src/old_mod.rs",
            "rust",
            "2026-10-14T00:00:01Z",
            "[]",
            r#"["legacy_helper"]"#,
            "[]",
            "[]",
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            ShiftEntry::render_side(&entries[0].before),
            "src/old_mod.rs:legacy_helper-"
        );
        assert!(entries[0].after.is_none());
        assert_eq!(entries[0].claim, "Removed legacy_helper");
    }

    #[test]
    fn symbol_added_and_removed_in_one_file_is_changed() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_full(
            &storage,
            "evo_30000003_1",
            "conv_1",
            "/repo/csr-engine/src/summarizer.rs",
            "rust",
            "2026-10-14T00:00:01Z",
            r#"["store_story"]"#,
            r#"["store_story"]"#,
            "[]",
            "[]",
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].before.is_none());
        assert_eq!(
            ShiftEntry::render_side(&entries[0].after),
            "src/summarizer.rs:store_story~"
        );
        assert_eq!(entries[0].claim, "Changed store_story");
    }

    #[test]
    fn single_symbol_keeps_the_symbol_endpoint() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_full(
            &storage,
            "evo_single_1",
            "conv_1",
            "/repo/csr-engine/src/story_map/derive.rs",
            "rust",
            "2026-10-14T00:00:01Z",
            r#"["derive_receipt_shifts"]"#,
            "[]",
            "[]",
            "[]",
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].before.is_none());
        assert_eq!(
            ShiftEntry::render_side(&entries[0].after),
            "story_map/derive.rs:derive_receipt_shifts+"
        );
        assert_eq!(entries[0].claim, "Added derive_receipt_shifts");
    }

    #[test]
    fn many_symbols_become_one_file_level_entry_with_counts() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_full(
            &storage,
            "evo_many_1",
            "conv_1",
            "/repo/csr-engine/src/codex_rollout.rs",
            "rust",
            "2026-10-14T00:00:01Z",
            r#"["c1", "c2", "a1", "a2", "a3"]"#,
            r#"["c1", "c2"]"#,
            "[]",
            "[]",
        );
        // The removal is a separate edit: one edit removing r1 while adding a1..a3 would be a swap.
        seed_evo_full(
            &storage,
            "evo_many_1b",
            "conv_1",
            "/repo/csr-engine/src/codex_rollout.rs",
            "rust",
            "2026-10-14T00:00:01Z",
            "[]",
            r#"["r1"]"#,
            "[]",
            "[]",
        );
        seed_evo_full(
            &storage,
            "evo_many_2",
            "conv_1",
            "/repo/csr-engine/src/foo.rs",
            "rust",
            "2026-10-14T00:00:02Z",
            r#"["f1", "f2", "f3", "f4", "f5"]"#,
            "[]",
            "[]",
            "[]",
        );
        seed_evo_full(
            &storage,
            "evo_many_3",
            "conv_1",
            "/repo/csr-engine/src/bar.rs",
            "rust",
            "2026-10-14T00:00:03Z",
            "[]",
            r#"["b1", "b2"]"#,
            "[]",
            "[]",
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 3);

        assert!(entries[0].before.is_none());
        assert_eq!(
            ShiftEntry::render_side(&entries[0].after),
            "src/codex_rollout.rs~"
        );
        assert_eq!(
            entries[0].claim,
            "Changed 2, added 3, removed 1 symbols in codex_rollout.rs"
        );

        assert!(entries[1].before.is_none());
        assert_eq!(ShiftEntry::render_side(&entries[1].after), "src/foo.rs~");
        assert_eq!(entries[1].claim, "Added 5 symbols in foo.rs");

        assert!(entries[2].before.is_none());
        assert_eq!(ShiftEntry::render_side(&entries[2].after), "src/bar.rs~");
        assert_eq!(entries[2].claim, "Removed 2 symbols in bar.rs");
    }

    #[test]
    fn file_touch_row_makes_an_edited_entry() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_full(
            &storage,
            "evo_40000004_1",
            "conv_1",
            "/repo/docs/architecture.md",
            "text",
            "2026-10-14T00:00:01Z",
            "[]",
            "[]",
            "[]",
            "[]",
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].before.is_none());
        assert_eq!(
            ShiftEntry::render_side(&entries[0].after),
            "docs/architecture.md~"
        );
        assert_eq!(entries[0].claim, "Edited architecture.md");
    }

    #[test]
    fn more_than_four_files_keep_the_top_four_deterministically() {
        let storage = Storage::open_memory().unwrap();
        // 1. Touch-only file (lowest rank: no symbol changes, 3 touches)
        for i in 0..3 {
            seed_evo_full(
                &storage,
                &format!("evo_t_{i}"),
                "conv_1",
                "/repo/docs/readme.md",
                "text",
                &format!("2026-10-14T00:00:0{i}Z"),
                "[]",
                "[]",
                "[]",
                "[]",
            );
        }
        // 2. Code file with 3 symbols (rank #1)
        seed_evo_full(
            &storage,
            "evo_c1",
            "conv_1",
            "/repo/src/alpha.rs",
            "rust",
            "2026-10-14T00:00:10Z",
            r#"["f1", "f2", "f3"]"#,
            "[]",
            "[]",
            "[]",
        );
        // 3. Code file with 1 symbol and 2 touches (rank #2)
        seed_evo_full(
            &storage,
            "evo_c2_a",
            "conv_1",
            "/repo/src/zeta.rs",
            "rust",
            "2026-10-14T00:00:11Z",
            r#"["z1"]"#,
            "[]",
            "[]",
            "[]",
        );
        seed_evo_full(
            &storage,
            "evo_c2_b",
            "conv_1",
            "/repo/src/zeta.rs",
            "rust",
            "2026-10-14T00:00:12Z",
            r#"["z1"]"#,
            "[]",
            "[]",
            "[]",
        );
        // 4 & 5. Two code files tied on (1 symbol, 1 touch), ordered by scope ascending:
        // src/beta.rs (rank #3) before src/gamma.rs (rank #4).
        seed_evo_full(
            &storage,
            "evo_c4",
            "conv_1",
            "/repo/src/gamma.rs",
            "rust",
            "2026-10-14T00:00:13Z",
            r#"["g1"]"#,
            "[]",
            "[]",
            "[]",
        );
        seed_evo_full(
            &storage,
            "evo_c3",
            "conv_1",
            "/repo/src/beta.rs",
            "rust",
            "2026-10-14T00:00:14Z",
            r#"["b1"]"#,
            "[]",
            "[]",
            "[]",
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 4);
        let scopes: Vec<String> = entries
            .iter()
            .map(|e| e.after.as_ref().unwrap().from.scope.clone())
            .collect();
        assert_eq!(
            scopes,
            vec!["src/alpha.rs", "src/zeta.rs", "src/beta.rs", "src/gamma.rs"]
        );
    }

    #[test]
    fn other_conversations_rows_are_ignored() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_full(
            &storage,
            "evo_other",
            "conv_other",
            "/repo/src/secret.rs",
            "rust",
            "2026-10-14T00:00:01Z",
            r#"["leaked"]"#,
            "[]",
            "[]",
            "[]",
        );
        seed_evo_full(
            &storage,
            "evo_mine",
            "conv_1",
            "/repo/src/mine.rs",
            "rust",
            "2026-10-14T00:00:02Z",
            r#"["kept"]"#,
            "[]",
            "[]",
            "[]",
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            ShiftEntry::render_side(&entries[0].after),
            "src/mine.rs:kept+"
        );
    }

    #[test]
    fn invalid_paths_and_symbols_are_skipped() {
        let storage = Storage::open_memory().unwrap();
        // Path with space -> invalid scope chars -> file skipped completely.
        seed_evo_full(
            &storage,
            "evo_bad_path",
            "conv_1",
            "/repo/my folder/bad file.rs",
            "rust",
            "2026-10-14T00:00:01Z",
            r#"["good_sym"]"#,
            "[]",
            "[]",
            "[]",
        );
        // Basename > 48 chars -> skipped.
        let long_base = format!("/repo/src/{}.rs", "a".repeat(50));
        seed_evo_full(
            &storage,
            "evo_long_base",
            "conv_1",
            &long_base,
            "rust",
            "2026-10-14T00:00:02Z",
            r#"["good_sym"]"#,
            "[]",
            "[]",
            "[]",
        );
        // Valid file whose 2-component path > 48 chars falls back to basename,
        // and invalid symbols are ignored while valid symbol is kept.
        let long_dir = format!("/repo/{}/ok_file.rs", "d".repeat(45));
        let overlong_sym = "s".repeat(41);
        let fa_json = serde_json::to_string(&vec![
            "bad sym with spaces".to_string(),
            overlong_sym,
            "valid_fn".to_string(),
        ])
        .unwrap();
        seed_evo_full(
            &storage,
            "evo_good_fallback",
            "conv_1",
            &long_dir,
            "rust",
            "2026-10-14T00:00:03Z",
            &fa_json,
            "[]",
            "[]",
            "[]",
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            ShiftEntry::render_side(&entries[0].after),
            "ok_file.rs:valid_fn+"
        );
        assert_eq!(entries[0].claim, "Added valid_fn");
    }

    #[test]
    fn claim_is_ascii_and_at_most_72_chars() {
        let storage = Storage::open_memory().unwrap();
        // 40-char symbol + 1000 other symbols: tests length cap; but even simpler,
        // test with a 40-char symbol and many other symbols so "Changed <40> and 12 more" is checked,
        // and test sanitize_claim directly for >72 chars.
        let sym40 = "a".repeat(40);
        let mut syms = vec![sym40.clone()];
        for i in 0..20 {
            syms.push(format!("extra_symbol_{i}"));
        }
        let fa = serde_json::to_string(&syms).unwrap();
        seed_evo_full(
            &storage,
            "evo_claim",
            "conv_1",
            "/repo/src/lib.rs",
            "rust",
            "2026-10-14T00:00:01Z",
            &fa,
            "[]",
            "[]",
            "[]",
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].claim.len() <= 72);
        assert!(entries[0]
            .claim
            .bytes()
            .all(|b| (0x20..=0x7E).contains(&b) && b != b'|'));

        let huge_raw = format!("Added {} and 999999999999999999999999999999 more", sym40);
        let capped = sanitize_claim(&huge_raw);
        assert!(capped.len() <= 72);
        assert!(!capped.ends_with(' '));
        let line = format!("shift|-|src/lib.rs:{sym40}+|{capped}");
        let parsed = parse_shift_line(&line).expect("must parse");
        assert!(parsed.claim.len() <= 72);
    }

    #[test]
    fn kill_switch_returns_nothing() {
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let storage = Storage::open_memory().unwrap();
        seed_evo_full(
            &storage,
            "evo_kill",
            "conv_1",
            "/repo/src/lib.rs",
            "rust",
            "2026-10-14T00:00:01Z",
            r#"["foo"]"#,
            "[]",
            "[]",
            "[]",
        );

        std::env::set_var("CSR_NO_STORY_MAP", "1");
        let off = derive_receipt_shifts(&storage, "conv_1");
        std::env::remove_var("CSR_NO_STORY_MAP");

        assert!(off.is_empty());
        let on = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(on.len(), 1);
    }

    #[test]
    fn empty_conversation_returns_nothing() {
        let storage = Storage::open_memory().unwrap();
        assert!(derive_receipt_shifts(&storage, "").is_empty());
        assert!(derive_receipt_shifts(&storage, "conv_with_no_rows").is_empty());
    }

    #[test]
    fn derived_entries_ground_against_their_own_rows() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_full(
            &storage,
            "evo_9f12a0c1_1",
            "conv_e2e",
            "/repo/csr-engine/src/summarizer.rs",
            "rust",
            "2026-10-14T00:00:01Z",
            r#"["store_story"]"#,
            r#"["store_story"]"#,
            "[]",
            "[]",
        );
        seed_evo_full(
            &storage,
            "evo_88776655_2",
            "conv_e2e",
            "/repo/csr-engine/src/story_map/derive.rs",
            "rust",
            "2026-10-14T00:00:02Z",
            r#"["derive_receipt_shifts"]"#,
            "[]",
            "[]",
            "[]",
        );
        seed_evo_full(
            &storage,
            "evo_deadbeef_3",
            "conv_e2e",
            "/repo/csr-engine/src/legacy.rs",
            "rust",
            "2026-10-14T00:00:03Z",
            "[]",
            r#"["old_fn"]"#,
            "[]",
            "[]",
        );
        seed_evo_full(
            &storage,
            "evo_1122aabb_4",
            "conv_e2e",
            "/repo/docs/notes.md",
            "text",
            "2026-10-14T00:00:04Z",
            "[]",
            "[]",
            "[]",
            "[]",
        );

        let derived = derive_receipt_shifts(&storage, "conv_e2e");
        assert_eq!(derived.len(), 4);

        let grounded = ground_shifts(&storage, "csr", "conv_e2e", &derived);
        assert_eq!(grounded.len(), 4);
        for g in &grounded {
            assert!(g.grounded, "entry {} must be grounded", g.claim);
            assert!(
                g.receipts.iter().any(|r| r.starts_with("evo:")),
                "entry {} must carry an evo: receipt",
                g.claim
            );
        }

        let map = build_story_map(
            None,
            "csr",
            "conv_e2e",
            1,
            1760400000,
            "receipts",
            25,
            &HashSet::new(),
            grounded,
        );
        save_story_map(&storage, &map).unwrap();

        let loaded = load_story_map(&storage, "conv_e2e").expect("saved story_map");
        assert_eq!(loaded.current.by, "receipts");
        assert_eq!(loaded.current.entries.len(), 4);
        assert!(loaded.current.entries.iter().all(|e| e.grounded));
    }

    const NONE: (&str, &str) = ("[]", "[]");

    #[allow(clippy::type_complexity)]
    fn seed_evo_swap(
        storage: &Storage,
        id: &str,
        file_path: &str,
        funcs: (&str, &str),
        types: (&str, &str),
        imports: (&str, &str),
    ) {
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO code_evolution (
                        id, session_id, project_name, file_path, language, timestamp,
                        functions_added, functions_removed, types_added, types_removed,
                        imports_added, imports_removed
                    ) VALUES (?1, 'conv_1', 'csr', ?2, 'rust', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        id,
                        file_path,
                        "2026-10-14T00:00:01Z",
                        funcs.0,
                        funcs.1,
                        types.0,
                        types.1,
                        imports.0,
                        imports.1
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn function_swap_in_one_edit_gets_a_before_and_an_after() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_aa000001_1",
            "/repo/csr-engine/src/storage/queries.rs",
            (
                r#"["conversation_scope_table_exists","get_conversation_scope","upsert_conversation_scope"]"#,
                r#"["insert_conversation_scope"]"#,
            ),
            NONE,
            NONE,
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        let swap = entries
            .iter()
            .find(|e| e.before.is_some())
            .expect("a swap entry with a before side");
        assert_eq!(
            ShiftEntry::render_side(&swap.before),
            "storage/queries.rs:insert_conversation_scope-"
        );
        let after = ShiftEntry::render_side(&swap.after);
        assert_eq!(after, "storage/queries.rs:upsert_conversation_scope+");
        assert_eq!(
            swap.claim,
            "Replaced insert_conversation_scope with 3 symbols in queries.rs"
        );
        assert!(swap.claim.len() <= 72);
    }

    #[test]
    fn short_names_keep_the_and_n_more_clause_and_the_file() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_aa000009_1",
            "/repo/csr-engine/src/storage/queries.rs",
            (r#"["a_scope","b_scope","c_scope"]"#, r#"["old_scope"]"#),
            NONE,
            NONE,
        );
        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(
            entries[0].claim,
            "Replaced old_scope with a_scope and 2 more in queries.rs"
        );
    }

    #[test]
    fn swap_ranks_ahead_of_the_plain_entry_for_the_same_file() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_aa000002_1",
            "/repo/csr-engine/src/storage/queries.rs",
            (r#"["get_scope"]"#, r#"["insert_scope"]"#),
            NONE,
            NONE,
        );
        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].claim,
            "Replaced insert_scope with get_scope in queries.rs"
        );
        assert_eq!(
            ShiftEntry::render_side(&entries[0].before),
            "storage/queries.rs:insert_scope-"
        );
        assert_eq!(
            ShiftEntry::render_side(&entries[0].after),
            "storage/queries.rs:get_scope+"
        );
        assert!(entries[1].before.is_none(), "today's entry comes second");
        assert_eq!(entries[1].claim, "Added 1, removed 1 symbols in queries.rs");
    }

    #[test]
    fn import_swap_in_the_same_module_names_what_it_adds() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_bb000001_1",
            "/repo/csr-engine/examples/hook_recall_replay.rs",
            NONE,
            NONE,
            (
                r#"["use rusqlite::{Connection, OpenFlags, OptionalExtension};"]"#,
                r#"["use rusqlite::{Connection, OpenFlags};"]"#,
            ),
        );
        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(
            entries[0].claim,
            "Swapped the rusqlite import for one that adds OptionalExtension"
        );
        assert_eq!(
            ShiftEntry::render_side(&entries[0].before),
            "examples/hook_recall_replay.rs:rusqlite~"
        );
        assert_eq!(
            ShiftEntry::render_side(&entries[0].after),
            "examples/hook_recall_replay.rs:OptionalExtension+"
        );
    }

    #[test]
    fn import_swap_across_modules_names_both() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_bb000002_1",
            "/repo/csr-engine/src/index.rs",
            NONE,
            NONE,
            (
                r#"["use indexmap::IndexSet;"]"#,
                r#"["use std::collections::HashSet;"]"#,
            ),
        );
        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(
            entries[0].claim,
            "Swapped the std::collections import for indexmap"
        );
        assert_eq!(
            ShiftEntry::render_side(&entries[0].before),
            "src/index.rs:std.collections-"
        );
        assert_eq!(
            ShiftEntry::render_side(&entries[0].after),
            "src/index.rs:indexmap+"
        );
    }

    #[test]
    fn multi_line_js_imports_and_dropped_items_read_in_plain_words() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_bb000003_1",
            "/repo/installer/postinstall.js",
            NONE,
            NONE,
            (
                "[\"import {\\n  existsSync,\\n  renameSync\\n} from 'fs';\"]",
                "[\"import {\\n  existsSync\\n} from 'fs';\"]",
            ),
        );
        seed_evo_swap(
            &storage,
            "evo_bb000004_1",
            "/repo/installer/cli.js",
            NONE,
            NONE,
            (
                "[\"import { join } from 'path';\"]",
                "[\"import { join, resolve } from 'path';\"]",
            ),
        );
        let entries = derive_receipt_shifts(&storage, "conv_1");
        let claims: Vec<&str> = entries.iter().map(|e| e.claim.as_str()).collect();
        assert!(
            claims.contains(&"Swapped the fs import for one that adds renameSync"),
            "{claims:?}"
        );
        assert!(
            claims.contains(&"Swapped the path import for one that drops resolve"),
            "{claims:?}"
        );
        let dropped = entries
            .iter()
            .find(|e| e.claim.ends_with("drops resolve"))
            .unwrap();
        assert_eq!(
            ShiftEntry::render_side(&dropped.before),
            "installer/cli.js:resolve-"
        );
        assert_eq!(
            ShiftEntry::render_side(&dropped.after),
            "installer/cli.js:path~"
        );
    }

    #[test]
    fn long_import_lists_fall_back_to_counts_instead_of_a_cut_word() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_bb000005_1",
            "/repo/installer/lib.js",
            NONE,
            NONE,
            (
                "[\"import { accessSync, closeSync, constants, fchmodSync, fstatSync, writeSync } from 'fs';\"]",
                "[\"import { accessSync, chmodSync, closeSync, constants, copyFileSync } from 'fs';\"]",
            ),
        );
        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(
            entries[0].claim,
            "Swapped the fs import for one that adds 3 names and drops 2 names"
        );
        assert!(entries[0].claim.len() <= 72);
    }

    #[test]
    fn nested_rust_use_groups_compare_by_their_top_level_items() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_bb000006_1",
            "/repo/csr-engine/src/io.rs",
            NONE,
            NONE,
            (
                r#"["use std::{io::{self, Read}, fs, path::Path};"]"#,
                r#"["use std::{io::{self, Read}, fs};"]"#,
            ),
        );
        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(
            entries[0].claim,
            "Swapped the std import for one that adds path::Path"
        );
    }

    #[test]
    fn a_fourth_import_tweak_does_not_crowd_out_a_function_swap() {
        let storage = Storage::open_memory().unwrap();
        for (i, f) in ["a", "b", "c"].iter().enumerate() {
            seed_evo_swap(
                &storage,
                &format!("evo_bb00001{i}_1"),
                &format!("/repo/csr-engine/src/{f}_use.rs"),
                NONE,
                NONE,
                (r#"["use x::{One, Two};"]"#, r#"["use x::{One};"]"#),
            );
        }
        seed_evo_swap(
            &storage,
            "evo_bb000019_1",
            "/repo/csr-engine/src/real.rs",
            (r#"["new_fn"]"#, r#"["old_fn"]"#),
            NONE,
            NONE,
        );
        let entries = derive_receipt_shifts(&storage, "conv_1");
        let claims: Vec<&str> = entries.iter().map(|e| e.claim.as_str()).collect();
        assert_eq!(entries.len(), 4);
        assert!(claims[0].starts_with("Swapped the x import"), "{claims:?}");
        assert!(claims[1].starts_with("Swapped the x import"), "{claims:?}");
        assert_eq!(claims[2], "Replaced old_fn with new_fn in real.rs");
        assert!(claims[3].starts_with("Swapped the x import"), "{claims:?}");
    }

    #[test]
    fn type_swap_reads_as_a_replacement() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_cc000001_1",
            "/repo/csr-engine/src/storage/mod.rs",
            NONE,
            (r#"["ConversationScopeRow"]"#, r#"["ScopeRow"]"#),
            NONE,
        );
        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(
            entries[0].claim,
            "Replaced ScopeRow with ConversationScopeRow in mod.rs"
        );
        assert_eq!(
            ShiftEntry::render_side(&entries[0].before),
            "storage/mod.rs:ScopeRow-"
        );
    }

    #[test]
    fn rows_without_a_same_edit_swap_give_todays_entries() {
        let storage = Storage::open_memory().unwrap();
        // One edit removes, a later edit adds: two edits, not a swap.
        seed_evo_swap(
            &storage,
            "evo_dd000001_1",
            "/repo/csr-engine/src/alpha.rs",
            ("[]", r#"["old_fn"]"#),
            NONE,
            NONE,
        );
        seed_evo_swap(
            &storage,
            "evo_dd000001_22",
            "/repo/csr-engine/src/alpha.rs",
            (r#"["new_fn"]"#, "[]"),
            NONE,
            NONE,
        );
        // Imports only added: not a swap.
        seed_evo_swap(
            &storage,
            "evo_dd000002_1",
            "/repo/csr-engine/src/beta.rs",
            NONE,
            NONE,
            (r#"["use std::fmt;"]"#, "[]"),
        );
        // A symbol edited in place is a change, not a swap.
        seed_evo_swap(
            &storage,
            "evo_dd000003_1",
            "/repo/csr-engine/src/gamma.rs",
            (r#"["same_fn"]"#, r#"["same_fn"]"#),
            NONE,
            NONE,
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        let claims: Vec<&str> = entries.iter().map(|e| e.claim.as_str()).collect();
        assert_eq!(
            claims,
            vec![
                "Added 1, removed 1 symbols in alpha.rs",
                "Changed same_fn",
                "Edited beta.rs"
            ]
        );
        assert!(entries.iter().all(|e| e.before.is_none()));
    }

    #[test]
    fn architecture_swaps_rank_first_then_function_swaps_then_the_rest() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_ee000001_1",
            "/repo/csr-engine/src/a_import.rs",
            NONE,
            NONE,
            (r#"["use b::Two;"]"#, r#"["use a::One;"]"#),
        );
        seed_evo_swap(
            &storage,
            "evo_ee000002_1",
            "/repo/csr-engine/src/b_type.rs",
            NONE,
            (r#"["NewType"]"#, r#"["OldType"]"#),
            NONE,
        );
        seed_evo_swap(
            &storage,
            "evo_ee000003_1",
            "/repo/csr-engine/src/c_fn.rs",
            (r#"["new_fn"]"#, r#"["old_fn"]"#),
            NONE,
            NONE,
        );
        seed_evo_swap(
            &storage,
            "evo_ee000004_1",
            "/repo/csr-engine/src/d_plain.rs",
            (r#"["a","b","c","d"]"#, "[]"),
            NONE,
            NONE,
        );

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].claim, "Swapped the a import for b");
        assert_eq!(
            entries[1].claim,
            "Replaced OldType with NewType in b_type.rs"
        );
        assert_eq!(entries[2].claim, "Replaced old_fn with new_fn in c_fn.rs");
        assert!(
            entries[3].before.is_none(),
            "a plain entry fills the last slot"
        );
    }

    #[test]
    fn test_only_swaps_rank_last_and_lose_the_cap() {
        let storage = Storage::open_memory().unwrap();
        // Names starting test_, and a file under tests/, both count as test-only.
        seed_evo_swap(
            &storage,
            "evo_ff000001_1",
            "/repo/csr-engine/src/search/mod.rs",
            (r#"["test_new_drift"]"#, r#"["test_old_drift"]"#),
            NONE,
            NONE,
        );
        seed_evo_swap(
            &storage,
            "evo_ff000002_1",
            "/repo/csr-engine/tests/integration.rs",
            (r#"["extract_rank_scores"]"#, r#"["regex_lite_extract"]"#),
            NONE,
            NONE,
        );
        seed_evo_swap(
            &storage,
            "evo_ff000003_1",
            "/repo/csr-engine/src/plain.rs",
            (r#"["p1"]"#, "[]"),
            NONE,
            NONE,
        );
        let entries = derive_receipt_shifts(&storage, "conv_1");
        let swap_positions: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.claim.starts_with("Replaced "))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(entries.len(), 4);
        assert_eq!(swap_positions, vec![3], "a test-only swap is last");
        assert_eq!(
            entries[3].claim,
            "Replaced test_old_drift with test_new_drift in mod.rs"
        );

        // Four plain entries push the test-only swaps out of the cap entirely.
        for (i, f) in ["w", "x", "y", "z"].iter().enumerate() {
            seed_evo_swap(
                &storage,
                &format!("evo_ff00001{i}_1"),
                &format!("/repo/csr-engine/src/{f}.rs"),
                (r#"["a1","a2","a3","a4","a5"]"#, "[]"),
                NONE,
                NONE,
            );
        }
        let capped = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(capped.len(), 4);
        assert!(capped.iter().all(|e| !e.claim.starts_with("Replaced ")));
    }

    #[test]
    fn swap_entries_ground_against_their_own_rows() {
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_99000001_1",
            "/repo/csr-engine/src/storage/queries.rs",
            (r#"["get_scope"]"#, r#"["insert_scope"]"#),
            NONE,
            NONE,
        );
        seed_evo_swap(
            &storage,
            "evo_99000002_1",
            "/repo/csr-engine/src/hooks/recall.rs",
            NONE,
            NONE,
            (
                r#"["use rusqlite::{Connection, OptionalExtension};"]"#,
                r#"["use rusqlite::Connection;"]"#,
            ),
        );
        let derived = derive_receipt_shifts(&storage, "conv_1");
        assert_eq!(derived.len(), 4);
        let grounded = ground_shifts(&storage, "csr", "conv_1", &derived);
        assert_eq!(grounded.len(), 4, "no swap entry is contradicted away");
        for g in &grounded {
            assert!(g.grounded, "{} must be grounded", g.claim);
            assert!(g.receipts.iter().any(|r| r.starts_with("evo:")));
        }
        let swap = grounded
            .iter()
            .find(|g| g.claim.starts_with("Replaced"))
            .unwrap();
        assert!(!swap.before.is_empty() && !swap.after.is_empty());
    }

    #[test]
    fn kill_switch_also_silences_swaps() {
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let storage = Storage::open_memory().unwrap();
        seed_evo_swap(
            &storage,
            "evo_88000001_1",
            "/repo/csr-engine/src/storage/queries.rs",
            (r#"["get_scope"]"#, r#"["insert_scope"]"#),
            NONE,
            NONE,
        );
        std::env::set_var("CSR_NO_STORY_MAP", "1");
        let off = derive_receipt_shifts(&storage, "conv_1");
        std::env::remove_var("CSR_NO_STORY_MAP");
        assert!(off.is_empty());
        assert!(derive_receipt_shifts(&storage, "conv_1")
            .iter()
            .any(|e| e.before.is_some()));
    }

    #[test]
    fn missing_table_fails_open() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|c| {
                c.execute("DROP TABLE code_evolution", [])?;
                Ok(())
            })
            .unwrap();

        let entries = derive_receipt_shifts(&storage, "conv_1");
        assert!(entries.is_empty());
    }
}
