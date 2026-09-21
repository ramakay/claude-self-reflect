//! Project scope for the semantic search tools.
//!
//! An unscoped search is meant to cover the project the client is working in.
//! The client's directory came only from `MCP_CLIENT_CWD`, which Claude Code
//! never sets, so from Claude Code every unscoped search ran across all
//! projects. It now falls back to `CLAUDE_PROJECT_DIR`
//! (`cross_project::resolve_client_dir`).
//!
//! Scoping by label alone would then hide every session started in a
//! subdirectory of the repository, which is stored under that subdirectory's
//! label. So the scope is the label plus the conversations the hook's folder
//! rule places in the client's repository (`hooks::scope_folder`).
//!
//! Only the chunk-search tools use this. The code-graph, file-ledger and
//! provenance tools keep resolving their project as before.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::hooks::scope_folder::{RepoConversations, MAX_PATHS_PER_CONVERSATION};
use crate::import::ConversationChunk;
use crate::search::cross_project::{resolve_client_dir, resolve_project_from_cwd};
use crate::storage::Storage;

/// A keyword search asks the index once per label the scope reaches. A
/// repository with sessions started in more subdirectories than this keeps the
/// first ones by name; the rest are still found by the semantic search.
const MAX_KEYWORD_LABELS: usize = 8;

/// How long one server process trusts its answer about which conversations
/// belong to the client's repository. The answer costs a full `import_state`
/// read and a handful of `git` calls; a new session shows up within this.
const REPO_CONVERSATIONS_TTL: Duration = Duration::from_secs(60);

type Cached = (String, Instant, Arc<RepoConversations>);

/// What one server process remembers about the client's repository.
#[derive(Default)]
struct ScopeCache {
    answer: Mutex<Option<Cached>>,
    /// One worker at a time. The rule gives up at its deadline, but a worker
    /// stuck on a dead mount stays behind. The flag is the worker's to clear,
    /// so while one is stuck no other is started and the scope is the last
    /// answer or the label alone.
    evaluating: AtomicBool,
}

fn process_cache() -> Arc<ScopeCache> {
    static CACHE: OnceLock<Arc<ScopeCache>> = OnceLock::new();
    Arc::clone(CACHE.get_or_init(Default::default))
}

/// What a project-scoped search may return.
pub struct SearchScope {
    pub project: String,
    /// Conversations stored under another label but filed from inside the
    /// client's repository, with the labels their chunks may carry. Never holds
    /// `project` itself, and is empty when the scope is not the client's own.
    repo_conversations: Arc<RepoConversations>,
}

impl SearchScope {
    /// Resolve the `project` argument of a search tool.
    ///
    /// * `"all"`: no scope.
    /// * a name: that label, widened only if it is the client's own project.
    /// * absent or empty: the client's own project, or no scope when the
    ///   client's directory is unknown.
    ///
    /// Returns the scope and the label to show for it.
    pub fn resolve(
        storage: &Storage,
        projects_dir: &Path,
        project: Option<&str>,
    ) -> (Option<Self>, String) {
        Self::resolve_with(
            storage,
            projects_dir,
            project,
            resolve_client_dir(),
            &process_cache(),
        )
    }

    fn resolve_with(
        storage: &Storage,
        projects_dir: &Path,
        project: Option<&str>,
        client_dir: Option<String>,
        cache: &Arc<ScopeCache>,
    ) -> (Option<Self>, String) {
        let own = client_dir
            .as_deref()
            .and_then(|dir| Some((resolve_project_from_cwd(dir)?, dir)));
        let project = match project {
            Some(p) if p.eq_ignore_ascii_case("all") => return (None, "all".to_string()),
            Some(p) if !p.is_empty() => p.to_string(),
            _ => match &own {
                Some((name, _)) => name.clone(),
                None => return (None, "all".to_string()),
            },
        };
        let repo_conversations = match &own {
            Some((name, dir)) if *name == project => {
                repo_conversations(storage, projects_dir, dir, name, cache)
            }
            _ => Arc::new(RepoConversations::new()),
        };
        let label = project.clone();
        (
            Some(Self {
                project,
                repo_conversations,
            }),
            label,
        )
    }

    /// Whether a chunk with this label and conversation is inside the scope.
    pub fn admits(&self, project_name: &str, conversation_id: &str) -> bool {
        project_name == self.project
            || self
                .repo_conversations
                .get(conversation_id)
                .is_some_and(|labels| labels.contains(project_name))
    }

    /// The keyword fallback inside the scope. The label query is the one that
    /// ran before scopes existed, so what it returns cannot be crowded out by
    /// anything added here; each label the scope reaches gets its own query,
    /// filtered in SQL before the limit, and keeps what the scope admits.
    pub fn keyword_search(
        &self,
        storage: &Storage,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ConversationChunk>> {
        let mut found = storage.fts5_search(query, limit, Some(self.project.as_str()))?;
        let labels: std::collections::BTreeSet<&str> = self
            .repo_conversations
            .values()
            .flatten()
            .map(String::as_str)
            .collect();
        let mut seen: HashSet<String> = found.iter().map(|c| c.id.clone()).collect();
        let mut extra = 0usize;
        for label in labels.into_iter().take(MAX_KEYWORD_LABELS) {
            for chunk in storage.fts5_search(query, limit, Some(label))? {
                if extra == limit {
                    return Ok(found);
                }
                if self.admits(&chunk.project_name, &chunk.conversation_id)
                    && seen.insert(chunk.id.clone())
                {
                    found.push(chunk);
                    extra += 1;
                }
            }
        }
        Ok(found)
    }

    /// Every chunk id inside the scope, for `search_chunks_filtered`.
    pub fn chunk_ids(&self, storage: &Storage) -> Result<HashSet<String>> {
        let mut ids: HashSet<String> = storage
            .get_chunk_ids_for_project(&self.project)?
            .into_iter()
            .collect();
        if !self.repo_conversations.is_empty() {
            let conversations: Vec<String> = self.repo_conversations.keys().cloned().collect();
            for (id, conversation_id, label) in
                storage.get_chunk_labels_for_conversations(&conversations)?
            {
                if self.admits(&label, &conversation_id) {
                    ids.insert(id);
                }
            }
        }
        Ok(ids)
    }
}

/// Conversations filed from inside the repository `client_dir` belongs to but
/// stored under a label other than `own_label`, remembered per server process
/// for [`REPO_CONVERSATIONS_TTL`]. Any failure is an empty set: the scope is
/// then the label alone, as it always was.
fn repo_conversations(
    storage: &Storage,
    projects_dir: &Path,
    client_dir: &str,
    own_label: &str,
    cache: &Arc<ScopeCache>,
) -> Arc<RepoConversations> {
    let mut stale = None;
    if let Ok(guard) = cache.answer.lock() {
        if let Some((dir, at, set)) = guard.as_ref() {
            if dir == client_dir {
                if at.elapsed() < REPO_CONVERSATIONS_TTL {
                    return Arc::clone(set);
                }
                stale = Some(Arc::clone(set));
            }
        }
    }
    if cache.evaluating.swap(true, Ordering::SeqCst) {
        // Another search is evaluating right now: the last answer, or none.
        return stale.unwrap_or_default();
    }
    let mut set = crate::hooks::scope_folder::conversations_in_client_repo(
        || match storage.all_import_paths(MAX_PATHS_PER_CONVERSATION) {
            Ok(paths) => Some(paths),
            Err(e) => {
                tracing::warn!(error = %e, "import_state read failed; project scope is the label only");
                None
            }
        },
        projects_dir,
        Path::new(client_dir),
        {
            let cache = Arc::clone(cache);
            move || cache.evaluating.store(false, Ordering::SeqCst)
        },
    );
    // Root sessions are in scope by label already; keep only what reaches past it.
    set.retain(|_, labels| {
        labels.remove(own_label);
        !labels.is_empty()
    });
    let set = Arc::new(set);
    if let Ok(mut guard) = cache.answer.lock() {
        *guard = Some((client_dir.to_string(), Instant::now(), Arc::clone(&set)));
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(storage: &Storage, id: &str, conversation_id: &str, label: &str, content: &str) {
        let chunk = crate::import::ConversationChunk {
            id: id.to_string(),
            conversation_id: conversation_id.to_string(),
            project_name: label.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: content.to_string(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
    }

    fn storage_with(rows: &[(&str, &str, &str)], import_rows: &[(&str, &str)]) -> Storage {
        let storage = Storage::open_memory().unwrap();
        for (id, conversation_id, label) in rows {
            let content = format!("work recorded under {label}");
            insert(&storage, id, conversation_id, label, &content);
        }
        for (file_path, conversation_id) in import_rows {
            storage
                .upsert_import_state_explicit(file_path, conversation_id, 1, "0")
                .unwrap();
        }
        storage
    }

    /// A cache of its own per test: the process-wide one would let two tests
    /// running side by side see each other's flag and answer.
    fn fresh() -> Arc<ScopeCache> {
        Arc::default()
    }

    fn sorted(ids: HashSet<String>) -> Vec<String> {
        let mut ids: Vec<String> = ids.into_iter().collect();
        ids.sort();
        ids
    }

    #[test]
    fn all_is_no_scope_and_an_unknown_client_directory_is_no_scope() {
        let storage = storage_with(&[], &[]);
        let cc = Path::new("/cc/projects");
        let (scope, label) = SearchScope::resolve_with(&storage, cc, Some("ALL"), None, &fresh());
        assert!(scope.is_none());
        assert_eq!(label, "all");
        let (scope, label) = SearchScope::resolve_with(&storage, cc, None, None, &fresh());
        assert!(scope.is_none());
        assert_eq!(label, "all");
        let (scope, label) = SearchScope::resolve_with(&storage, cc, Some(""), None, &fresh());
        assert!(scope.is_none());
        assert_eq!(label, "all");
    }

    #[test]
    fn a_named_project_that_is_not_the_clients_own_is_the_label_and_nothing_else() {
        let storage = storage_with(
            &[("a", "s-a", "other"), ("b", "s-b", "repo")],
            &[("/cc/projects/-u-projects-other/s-a.jsonl", "s-a")],
        );
        let (scope, label) = SearchScope::resolve_with(
            &storage,
            Path::new("/cc/projects"),
            Some("other"),
            Some("/u/projects/repo".to_string()),
            &fresh(),
        );
        let scope = scope.unwrap();
        assert_eq!(label, "other");
        assert_eq!(sorted(scope.chunk_ids(&storage).unwrap()), ["a"]);
        assert!(scope.admits("other", "s-a"));
        assert!(!scope.admits("repo", "s-b"));
    }

    /// The point of the module: with a real repository on disk, an unscoped
    /// search from its subdirectory covers the root sessions (by label) and the
    /// subdirectory sessions (by folder), and leaves a sibling repository out.
    #[test]
    fn the_clients_own_project_covers_its_subdirectory_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("projects/repo");
        let tools = root.join("projects/repo-tools");
        std::fs::create_dir_all(repo.join("engine")).unwrap();
        std::fs::create_dir_all(&tools).unwrap();
        let git_init = |dir: &Path| {
            let mut cmd = std::process::Command::new("git");
            for (k, _) in std::env::vars_os() {
                if k.to_string_lossy().starts_with("GIT_") {
                    cmd.env_remove(&k);
                }
            }
            cmd.arg("init")
                .arg("-q")
                .arg(dir)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !git_init(&repo) || !git_init(&tools) {
            return; // git unavailable in this environment
        }
        let folder = |dir: &Path| {
            crate::search::cross_project::encode_project_folder(&dir.to_string_lossy())
        };
        let engine_path = format!("/cc/projects/{}/s-sub.jsonl", folder(&repo.join("engine")));
        let tools_path = format!("/cc/projects/{}/s-tools.jsonl", folder(&tools));
        let storage = storage_with(
            &[
                ("root-chunk", "s-root", "repo"),
                ("sub-chunk", "s-sub", "repo-engine"),
                // Same conversation id, another file's label: overwritten by a
                // same-stem transcript from elsewhere. Stays out.
                ("overwritten-chunk", "s-sub", "elsewhere"),
                ("tools-chunk", "s-tools", "repo-tools"),
            ],
            &[
                (engine_path.as_str(), "s-sub"),
                (tools_path.as_str(), "s-tools"),
            ],
        );
        let client_dir = repo.join("engine").to_string_lossy().into_owned();
        let (scope, label) = SearchScope::resolve_with(
            &storage,
            Path::new("/cc/projects"),
            None,
            Some(client_dir),
            &fresh(),
        );
        let scope = scope.unwrap();
        assert_eq!(label, "repo");
        assert_eq!(
            sorted(scope.chunk_ids(&storage).unwrap()),
            ["root-chunk", "sub-chunk"]
        );
        assert!(scope.admits("repo-engine", "s-sub"));
        assert!(!scope.admits("elsewhere", "s-sub"));
        assert!(!scope.admits("repo-tools", "s-tools"));
    }

    /// The keyword fallback: better-ranked matches from other projects, however
    /// many, cannot push out the scope's own, because every query is filtered
    /// by label before its limit. A chunk that shares the subdirectory's label
    /// but whose conversation the folder rule did not place stays out.
    #[test]
    fn keyword_matches_elsewhere_cannot_crowd_out_the_scope() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("projects/kwrepo");
        std::fs::create_dir_all(repo.join("engine")).unwrap();
        let mut cmd = std::process::Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
        if !cmd
            .arg("init")
            .arg("-q")
            .arg(&repo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return; // git unavailable in this environment
        }
        let folder = |dir: &Path| {
            crate::search::cross_project::encode_project_folder(&dir.to_string_lossy())
        };
        let storage = storage_with(&[], &[]);
        insert(&storage, "own", "s-root", "kwrepo", "the quuxflag switch");
        insert(
            &storage,
            "sub",
            "s-sub",
            "kwrepo-engine",
            "quuxflag in the engine",
        );
        insert(
            &storage,
            "unplaced",
            "s-lost",
            "kwrepo-engine",
            "quuxflag too",
        );
        for i in 0..30 {
            let id = format!("other-{i}");
            insert(
                &storage,
                &id,
                &id,
                "elsewhere",
                "quuxflag quuxflag quuxflag quuxflag",
            );
        }
        let root_path = format!("/cc/projects/{}/s-root.jsonl", folder(&repo));
        let sub_path = format!("/cc/projects/{}/s-sub.jsonl", folder(&repo.join("engine")));
        for (path, id) in [(&root_path, "s-root"), (&sub_path, "s-sub")] {
            storage
                .upsert_import_state_explicit(path, id, 1, "0")
                .unwrap();
        }

        let client_dir = repo.to_string_lossy().into_owned();
        let (scope, _) = SearchScope::resolve_with(
            &storage,
            Path::new("/cc/projects"),
            None,
            Some(client_dir),
            &fresh(),
        );
        let scope = scope.unwrap();
        // The root session is in scope by label and is not carried as widened.
        assert!(!scope.repo_conversations.contains_key("s-root"));
        let mut found: Vec<String> = scope
            .keyword_search(&storage, "quuxflag", 5)
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        found.sort();
        assert_eq!(found, ["own", "sub"]);
    }
}
