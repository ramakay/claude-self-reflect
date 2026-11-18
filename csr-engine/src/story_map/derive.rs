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
#[path = "derive_tests.rs"]
mod tests;
