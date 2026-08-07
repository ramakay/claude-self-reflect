//! Deterministic release-tier episode ancestry labels for TAD v2.
//!
//! Evidence is deliberately narrow: transcript and git attribution rows must
//! meet on the same code node, and that node must carry one repository root.
//! Git is consulted only by [`refresh_ancestry_cache`]; retrieval reads the
//! resulting SQLite cache through [`Storage::ancestry_labels_for_conversations`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, params_from_iter};

use super::Storage;

/// The daemon refreshes hourly. Three hours covers a couple of missed cycles
/// without letting failed invalidation silently demote data for a full day.
pub const ANCESTRY_CACHE_TTL_HOURS: i64 = 3;
/// Small tolerance for clock skew between the cache writer and reader.
pub const ANCESTRY_CACHE_FUTURE_SKEW_MINUTES: i64 = 5;
const INVALIDATION_ATTEMPTS: usize = 3;
const INVALIDATION_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(25);
/// Git is deliberately synchronous in the daemon's blocking pool. Each
/// subprocess is killed and reaped after this bound, which also bounds daemon
/// shutdown without adding an async process dependency.
const GIT_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AncestryState {
    Shipped,
    Unreleased,
}

impl AncestryState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Shipped => "shipped",
            Self::Unreleased => "unreleased",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "shipped" => Ok(Self::Shipped),
            "unreleased" => Ok(Self::Unreleased),
            other => Err(anyhow!("invalid ancestry cache state: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncestryLabel {
    pub conversation_id: String,
    pub state: AncestryState,
    pub release_tag: Option<String>,
    pub releases_behind: u32,
    pub repository: String,
    pub refreshed_at: String,
}

impl AncestryLabel {
    /// Release distance used by scoring. Unreleased work is deliberately
    /// neutral; shipped-in-current-release (`Some(0)`) is also score-neutral.
    pub fn releases_behind_for_decay(&self) -> Option<u32> {
        (self.state == AncestryState::Shipped).then_some(self.releases_behind)
    }
}

#[derive(Debug)]
struct EvidenceRow {
    conversation_id: String,
    commit_sha: Option<String>,
    repository: Option<String>,
}

#[derive(Debug)]
struct RepoReleases {
    tags: Vec<String>,
    claimed: BTreeMap<String, usize>,
    head_commits: BTreeSet<String>,
}

/// Refresh using the current UTC timestamp. Intended for daemon cycles.
pub fn refresh_ancestry_cache(storage: &Storage) -> Result<usize> {
    refresh_ancestry_cache_at(storage, &chrono::Utc::now().to_rfc3339())
}

/// Deterministic refresh seam used by tests and daemon code. `refreshed_at`
/// is normalized to RFC3339 UTC; all other row bytes derive from sorted git
/// and SQLite inputs.
pub fn refresh_ancestry_cache_at(storage: &Storage, refreshed_at: &str) -> Result<usize> {
    match try_refresh_ancestry_cache_at(storage, refreshed_at) {
        Ok(count) => Ok(count),
        Err(error) => {
            // A failed refresh must not leave a prior shipped label active:
            // publish an empty cache generation so retrieval fails open.
            invalidate_ancestry_cache(storage)
                .context("failed to invalidate ancestry cache after retries")?;
            Err(error)
        }
    }
}

/// Best-effort fail-open publication with short retries for transient SQLite
/// contention. Retrieval's TTL remains the final bound if all attempts fail.
pub(crate) fn invalidate_ancestry_cache(storage: &Storage) -> Result<()> {
    let mut last_error = None;
    for attempt in 0..INVALIDATION_ATTEMPTS {
        match replace_cache(storage, &[]) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < INVALIDATION_ATTEMPTS {
            std::thread::sleep(INVALIDATION_RETRY_DELAY);
        }
    }
    Err(last_error.expect("at least one invalidation attempt"))
}

fn try_refresh_ancestry_cache_at(storage: &Storage, refreshed_at: &str) -> Result<usize> {
    let mut refresh = prepare_ancestry_refresh(storage, refreshed_at)?;
    for repository in refresh.repositories() {
        refresh = refresh.walk_repository(&repository);
    }
    refresh.publish(storage)
}

pub(crate) struct PreparedAncestryRefresh {
    evidence: Vec<EvidenceRow>,
    refreshed_at: String,
    repo_indexes: BTreeMap<String, RepoReleases>,
}

pub(crate) fn prepare_ancestry_refresh(
    storage: &Storage,
    refreshed_at: &str,
) -> Result<PreparedAncestryRefresh> {
    let refreshed_at = chrono::DateTime::parse_from_rfc3339(refreshed_at)
        .context("invalid ancestry refresh timestamp")?
        .with_timezone(&chrono::Utc)
        .to_rfc3339();
    let evidence = load_evidence(storage)?;
    Ok(PreparedAncestryRefresh {
        evidence,
        refreshed_at,
        repo_indexes: BTreeMap::new(),
    })
}

impl PreparedAncestryRefresh {
    pub(crate) fn repositories(&self) -> Vec<String> {
        self.evidence
            .iter()
            .filter_map(|row| row.repository.as_ref())
            .filter(|repository| !repository.is_empty())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// One repository walk. Repository-specific failures stay neutral and do
    /// not prevent other repositories from publishing their evidence.
    pub(crate) fn walk_repository(mut self, repository: &str) -> Self {
        if let Ok(index) = load_repo_releases(Path::new(repository)) {
            self.repo_indexes.insert(repository.to_string(), index);
        }
        self
    }

    pub(crate) fn publish(self, storage: &Storage) -> Result<usize> {
        let labels = build_labels(&self.evidence, &self.refreshed_at, &self.repo_indexes);
        replace_cache(storage, &labels)?;
        Ok(labels.len())
    }
}

fn load_evidence(storage: &Storage) -> Result<Vec<EvidenceRow>> {
    storage.with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT transcript.source_id, git.source_id, nodes.repo_root
             FROM code_node_attribution AS transcript
             LEFT JOIN code_node_attribution AS git
               ON git.node_id = transcript.node_id AND git.channel = 'git'
             LEFT JOIN code_nodes AS nodes ON nodes.id = transcript.node_id
             WHERE transcript.channel = 'transcript'
             ORDER BY transcript.source_id, git.source_id, nodes.repo_root",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(EvidenceRow {
                conversation_id: row.get(0)?,
                commit_sha: row.get(1)?,
                repository: row.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    })
}

fn build_labels(
    evidence: &[EvidenceRow],
    refreshed_at: &str,
    repo_indexes: &BTreeMap<String, RepoReleases>,
) -> Vec<AncestryLabel> {
    let mut invalid_conversations = BTreeSet::new();
    let mut conversation_repos: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut conversation_commit_links: BTreeMap<String, BTreeSet<(String, String)>> =
        BTreeMap::new();
    // Commit identity is the normalized object id, not `(checkout, object
    // id)`: two clones of the same repository must not turn one fan-out
    // commit into two apparently unambiguous facts.
    let mut commit_sessions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for row in evidence {
        let Some(repository) = row.repository.as_ref().filter(|r| !r.is_empty()) else {
            invalid_conversations.insert(row.conversation_id.clone());
            continue;
        };
        let Some(sha) = row.commit_sha.as_deref().and_then(normalized_full_sha) else {
            invalid_conversations.insert(row.conversation_id.clone());
            continue;
        };
        conversation_repos
            .entry(row.conversation_id.clone())
            .or_default()
            .insert(repository.clone());
        conversation_commit_links
            .entry(row.conversation_id.clone())
            .or_default()
            .insert((repository.clone(), sha.clone()));
        commit_sessions
            .entry(sha)
            .or_default()
            .insert(row.conversation_id.clone());
    }

    for (conversation, repositories) in &conversation_repos {
        if repositories.len() != 1 {
            invalid_conversations.insert(conversation.clone());
        }
    }

    // A commit linked to multiple transcript sessions is ambiguous. Because
    // it may be newer than every unique link, every touched conversation must
    // fail open for this generation rather than merely dropping that SHA.
    for sessions in commit_sessions
        .values()
        .filter(|sessions| sessions.len() > 1)
    {
        invalid_conversations.extend(sessions.iter().cloned());
    }
    let unambiguous_commits: BTreeSet<String> = commit_sessions
        .iter()
        .filter(|(_, sessions)| sessions.len() == 1)
        .map(|(sha, _)| sha.clone())
        .collect();
    let mut conversation_commits: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    for (conversation, links) in conversation_commit_links {
        if invalid_conversations.contains(&conversation) {
            continue;
        }
        for (repository, sha) in links {
            if !unambiguous_commits.contains(&sha) {
                continue;
            }
            conversation_commits
                .entry(conversation.clone())
                .or_default()
                .insert((repository, sha));
        }
    }

    let mut labels = Vec::new();
    for (conversation_id, commits) in conversation_commits {
        if invalid_conversations.contains(&conversation_id) || commits.is_empty() {
            continue;
        }
        let repository = commits
            .iter()
            .next()
            .map(|(repo, _)| repo.clone())
            .expect("non-empty commits");
        if commits.iter().any(|(repo, _)| repo != &repository) {
            continue;
        }
        let Some(releases) = repo_indexes.get(&repository) else {
            continue;
        };

        let mut newest_release: Option<usize> = None;
        let mut unreleased = false;
        let mut unknown = false;
        for (_, sha) in &commits {
            if let Some(&release_index) = releases.claimed.get(sha) {
                newest_release =
                    Some(newest_release.map_or(release_index, |i| i.max(release_index)));
            } else if releases.head_commits.contains(sha) {
                unreleased = true;
            } else {
                // A syntactically valid SHA that is neither released nor at
                // HEAD cannot support a demotion.
                unknown = true;
            }
        }
        if unknown {
            continue;
        }

        if let Some(release_index) = newest_release {
            labels.push(AncestryLabel {
                conversation_id,
                state: AncestryState::Shipped,
                release_tag: Some(releases.tags[release_index].clone()),
                releases_behind: (releases.tags.len() - release_index - 1) as u32,
                repository,
                refreshed_at: refreshed_at.to_string(),
            });
        } else if unreleased {
            labels.push(AncestryLabel {
                conversation_id,
                state: AncestryState::Unreleased,
                release_tag: None,
                releases_behind: 0,
                repository,
                refreshed_at: refreshed_at.to_string(),
            });
        }
    }
    labels
}

fn normalized_full_sha(value: &str) -> Option<String> {
    let value = value.trim();
    ((value.len() == 40 || value.len() == 64) && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn load_repo_releases(repo: &Path) -> Result<RepoReleases> {
    if !repo.is_dir() {
        return Err(anyhow!("repository path is absent: {}", repo.display()));
    }
    if git_lines(repo, &["rev-parse", "--is-inside-work-tree"])? != ["true"] {
        return Err(anyhow!("not a git worktree: {}", repo.display()));
    }
    // Detached HEAD makes the repository's release episode ambiguous.
    git_lines(repo, &["symbolic-ref", "-q", "--short", "HEAD"])?;

    let tags = git_lines(repo, &["tag", "--list", "v*", "--sort=version:refname"])?;
    if tags.is_empty() {
        return Err(anyhow!("no v* release tags: {}", repo.display()));
    }

    let mut claimed = BTreeMap::new();
    for (index, tag) in tags.iter().enumerate() {
        let range = if index == 0 {
            tag.clone()
        } else {
            format!("{}..{}", tags[index - 1], tag)
        };
        for sha in git_lines(repo, &["rev-list", &range])? {
            claimed.entry(sha.to_ascii_lowercase()).or_insert(index);
        }
    }
    let head_commits = git_lines(repo, &["rev-list", "HEAD"])?
        .into_iter()
        .map(|sha| sha.to_ascii_lowercase())
        .collect();
    Ok(RepoReleases {
        tags,
        claimed,
        head_commits,
    })
}

fn git_lines(repo: &Path, args: &[&str]) -> Result<Vec<String>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run git in {}", repo.display()))?;
    let mut stdout = child.stdout.take().context("git stdout pipe missing")?;
    let mut stderr = child.stderr.take().context("git stderr pipe missing")?;
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= GIT_COMMAND_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(anyhow!(
                "git {args:?} timed out after {}s in {}",
                GIT_COMMAND_TIMEOUT.as_secs(),
                repo.display()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow!("git stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow!("git stderr reader panicked"))??;
    if !status.success() {
        return Err(anyhow!(
            "git {args:?} failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    Ok(String::from_utf8(stdout)
        .context("git output was not UTF-8")?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn replace_cache(storage: &Storage, labels: &[AncestryLabel]) -> Result<()> {
    storage.with_connection(|conn| {
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM conversation_ancestry_cache", [])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO conversation_ancestry_cache
                 (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for label in labels {
                stmt.execute(params![
                    label.conversation_id,
                    label.state.as_str(),
                    label.release_tag,
                    i64::from(label.releases_behind),
                    label.repository,
                    label.refreshed_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
}

impl Storage {
    /// One indexed SQLite read for the supplied retrieval candidate set.
    pub fn ancestry_labels_for_conversations(
        &self,
        conversation_ids: &[String],
    ) -> Result<HashMap<String, AncestryLabel>> {
        if conversation_ids.is_empty() {
            return Ok(HashMap::new());
        }
        self.with_connection(|conn| {
            let placeholders = std::iter::repeat_n("?", conversation_ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT conversation_id, state, release_tag, releases_behind,
                        repository, refreshed_at
                 FROM conversation_ancestry_cache
                 WHERE conversation_id IN ({placeholders})
                 ORDER BY conversation_id"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(conversation_ids), |row| {
                let state: String = row.get(1)?;
                let releases_behind: i64 = row.get(3)?;
                Ok((
                    state,
                    releases_behind,
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?;
            let mut labels = HashMap::new();
            let now = chrono::Utc::now();
            let oldest_allowed = now - chrono::Duration::hours(ANCESTRY_CACHE_TTL_HOURS);
            let newest_allowed =
                now + chrono::Duration::minutes(ANCESTRY_CACHE_FUTURE_SKEW_MINUTES);
            for row in rows {
                let (
                    state,
                    releases_behind,
                    conversation_id,
                    release_tag,
                    repository,
                    refreshed_at,
                ) = row?;
                let Ok(refreshed) = chrono::DateTime::parse_from_rfc3339(&refreshed_at) else {
                    continue;
                };
                let refreshed = refreshed.with_timezone(&chrono::Utc);
                if refreshed < oldest_allowed || refreshed > newest_allowed {
                    continue;
                }
                let releases_behind = u32::try_from(releases_behind)
                    .map_err(|_| anyhow!("negative/oversized releases_behind"))?;
                labels.insert(
                    conversation_id.clone(),
                    AncestryLabel {
                        conversation_id,
                        state: AncestryState::parse(&state)?,
                        release_tag,
                        releases_behind,
                        repository,
                        refreshed_at,
                    },
                );
            }
            Ok(labels)
        })
    }

    pub fn ancestry_cache_count(&self) -> Result<i64> {
        self.with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM conversation_ancestry_cache",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    use super::*;

    #[test]
    fn failed_invalidation_is_retried_after_transient_sqlite_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ancestry.sqlite3");
        let storage = Arc::new(Storage::open(&db_path).unwrap());
        storage
            .with_connection(|conn| {
                conn.busy_timeout(Duration::ZERO)?;
                conn.execute(
                    "INSERT INTO conversation_ancestry_cache
                     (conversation_id, state, release_tag, releases_behind,
                      repository, refreshed_at)
                     VALUES ('stale', 'shipped', 'v1.0.0', 5, '/repo', ?1)",
                    [chrono::Utc::now().to_rfc3339()],
                )?;
                Ok(())
            })
            .unwrap();

        let blocker = rusqlite::Connection::open(&db_path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let worker = {
            let storage = storage.clone();
            std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                refresh_ancestry_cache_at(&storage, "not-a-timestamp")
            })
        };
        started_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(10));
        blocker.execute_batch("COMMIT").unwrap();

        assert!(worker.join().unwrap().is_err());
        assert_eq!(storage.ancestry_cache_count().unwrap(), 0);
    }
}
