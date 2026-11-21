//! Extractive session card. Built from the conversation's stored episodes, so
//! an incremental parse still yields a whole-session card. No model.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use rusqlite::{params, Connection};

use crate::search::identifiers::extract_identifiers;

const TITLE_CHARS: usize = 120;
const MAX_IDENTIFIERS: usize = 30;
const MAX_DECISIONS: usize = 12;
const MAX_RECEIPTS: usize = 40;

static DECISION_CUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(decided|decision|we(?:'ll| will) go with|going with|chose|chosen|settled on|let's use|we(?:'ll| will) use|switch(?:ed|ing)? to|instead of)\b",
    )
    .expect("decision regex")
});
static COMMIT_SHA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\bcommit[ted]* |\bsha:? |\[[\w/.\-]+ )([0-9a-f]{7,40})\b")
        .expect("sha regex")
});
static PR_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\bPR #?|pull/|\(#)(\d{1,6})\b").expect("pr regex"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub conversation_id: String,
    pub project_name: String,
    pub title: String,
    pub identifiers: Vec<String>,
    pub decisions: Vec<String>,
    pub task_state: String,
    pub receipts: Vec<String>,
    pub first_ts: String,
    pub last_ts: String,
    pub episode_count: i64,
}

/// Card from `(timestamp, text)` episode rows in ordinal order.
pub fn card_from_episodes(
    conversation_id: &str,
    project: &str,
    rows: &[(String, String)],
) -> Option<Card> {
    if rows.is_empty() {
        return None;
    }
    let title = rows
        .iter()
        .filter_map(|(_, text)| text.strip_prefix("U: "))
        .find_map(|t| {
            t.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with("A: "))
                .map(|l| l.chars().take(TITLE_CHARS).collect::<String>())
        })
        .unwrap_or_default();

    let mut ident_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut decisions: Vec<String> = Vec::new();
    let mut seen_decisions: BTreeSet<String> = BTreeSet::new();
    let mut receipts: BTreeSet<String> = BTreeSet::new();
    let mut task_state = String::new();
    for (_, text) in rows {
        for (term, _) in extract_identifiers(text) {
            *ident_counts.entry(term).or_default() += 1;
        }
        for line in text.lines() {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("[TodoWrite: ") {
                task_state = format!("TodoWrite: {}", rest.trim_end_matches(']'));
            } else if let Some(rest) = l.strip_prefix("[TaskCreate: ") {
                task_state = format!("TaskCreate: {}", rest.trim_end_matches(']'));
            } else if let Some(rest) = l.strip_prefix("[TaskUpdate: ") {
                task_state = format!("TaskUpdate: {}", rest.trim_end_matches(']'));
            }
            let n = l.chars().count();
            if decisions.len() < MAX_DECISIONS
                && (20..=300).contains(&n)
                && !l.starts_with('[')
                && DECISION_CUE.is_match(l)
                && seen_decisions.insert(l.to_string())
            {
                decisions.push(l.to_string());
            }
        }
        for c in COMMIT_SHA.captures_iter(text) {
            receipts.insert(format!("commit:{}", c[1].to_lowercase()));
        }
        for c in PR_NUMBER.captures_iter(text) {
            receipts.insert(format!("pr:{}", &c[1]));
        }
    }
    let mut idents: Vec<(String, usize)> = ident_counts.into_iter().collect();
    idents.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Some(Card {
        conversation_id: conversation_id.to_string(),
        project_name: project.to_string(),
        title,
        identifiers: idents
            .into_iter()
            .take(MAX_IDENTIFIERS)
            .map(|(t, _)| t)
            .collect(),
        decisions,
        task_state,
        receipts: receipts.into_iter().take(MAX_RECEIPTS).collect(),
        first_ts: rows.first().map(|r| r.0.clone()).unwrap_or_default(),
        last_ts: rows.last().map(|r| r.0.clone()).unwrap_or_default(),
        episode_count: rows.len() as i64,
    })
}

/// Rebuild the card for one conversation from its stored episodes. Returns
/// whether a card exists afterwards.
pub fn rebuild_card(conn: &Connection, conversation_id: &str) -> Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT project_name, timestamp, text FROM episodes
         WHERE conversation_id = ?1 ORDER BY ordinal",
    )?;
    let mut project = String::new();
    let rows: Vec<(String, String)> = stmt
        .query_map([conversation_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(p, ts, text)| {
            project = p;
            (ts, text)
        })
        .collect();
    let Some(card) = card_from_episodes(conversation_id, &project, &rows) else {
        conn.execute(
            "DELETE FROM session_cards WHERE conversation_id = ?1",
            [conversation_id],
        )?;
        return Ok(false);
    };
    let j = |v: &Vec<String>| serde_json::to_string(v).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO session_cards
            (conversation_id, project_name, title, identifiers, decisions, task_state,
             receipts, first_ts, last_ts, episode_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(conversation_id) DO UPDATE SET
            project_name = excluded.project_name, title = excluded.title,
            identifiers = excluded.identifiers, decisions = excluded.decisions,
            task_state = excluded.task_state, receipts = excluded.receipts,
            first_ts = excluded.first_ts, last_ts = excluded.last_ts,
            episode_count = excluded.episode_count",
        params![
            card.conversation_id,
            card.project_name,
            card.title,
            j(&card.identifiers),
            j(&card.decisions),
            card.task_state,
            j(&card.receipts),
            card.first_ts,
            card.last_ts,
            card.episode_count
        ],
    )?;
    Ok(true)
}
