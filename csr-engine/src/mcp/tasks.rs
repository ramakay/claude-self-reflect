//! MCP Tasks — async wrappers for heavy search operations.
//!
//! Wraps slow tools (reflect_on_past, search_by_concept, search_insights,
//! search_by_recency) in tokio tasks with status tracking. Clients can
//! poll progress or cancel running operations.
//!
//! Task lifecycle: Working → Completed | Failed | Cancelled
//! Default TTL: 30 seconds after completion.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use rmcp::model::{CallToolResult, Content, Task, TaskStatus};

/// Default TTL for completed tasks (30 seconds in milliseconds).
const DEFAULT_TTL_MS: u64 = 30_000;

/// Default poll interval suggestion (500ms).
const DEFAULT_POLL_MS: u64 = 500;

/// Tools eligible for async task execution.
const TASKABLE_TOOLS: &[&str] = &[
    "csr_reflect_on_past",
    "csr_search_by_concept",
    "csr_search_insights",
    "search_by_recency",
];

/// Check if a tool name is eligible for async task execution.
pub fn is_taskable(tool_name: &str) -> bool {
    TASKABLE_TOOLS.contains(&tool_name)
}

/// Internal state for a managed task.
#[derive(Debug)]
struct ManagedTask {
    task: Task,
    result: Option<CallToolResult>,
    cancel_token: tokio::sync::watch::Sender<bool>,
}

/// Task manager that tracks async tool executions.
#[derive(Clone)]
pub struct TaskManager {
    tasks: Arc<Mutex<HashMap<String, ManagedTask>>>,
    counter: Arc<std::sync::atomic::AtomicU64>,
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            counter: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Generate a unique task ID.
    fn next_id(&self) -> String {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("csr-task-{}", n)
    }

    /// Enqueue a tool call as an async task. Returns the Task immediately.
    /// The actual work runs in a spawned tokio task.
    pub async fn enqueue<F>(&self, tool_name: &str, work: F) -> Task
    where
        F: std::future::Future<Output = Result<CallToolResult, rmcp::ErrorData>> + Send + 'static,
    {
        let task_id = self.next_id();
        let now = chrono::Utc::now().to_rfc3339();

        let task = Task::new(task_id.clone(), TaskStatus::Working, now.clone(), now)
            .with_ttl(DEFAULT_TTL_MS)
            .with_poll_interval(DEFAULT_POLL_MS)
            .with_status_message(format!("Running {}", tool_name));

        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);

        let managed = ManagedTask {
            task: task.clone(),
            result: None,
            cancel_token: cancel_tx,
        };

        self.tasks.lock().await.insert(task_id.clone(), managed);

        // Spawn the actual work
        let tasks = self.tasks.clone();
        let tid = task_id.clone();
        tokio::spawn(async move {
            // Use changed() instead of wait_for() to avoid !Send Ref across await
            let outcome = tokio::select! {
                result = work => Some(result),
                _ = async {
                    loop {
                        if cancel_rx.changed().await.is_err() { break; }
                        if *cancel_rx.borrow() { break; }
                    }
                } => None,
            };

            let mut map = tasks.lock().await;
            if let Some(mt) = map.get_mut(&tid) {
                let now = chrono::Utc::now().to_rfc3339();
                match outcome {
                    Some(Ok(result)) => {
                        mt.task.status = TaskStatus::Completed;
                        mt.task.status_message = Some("Done".to_string());
                        mt.result = Some(result);
                    }
                    Some(Err(e)) => {
                        mt.task.status = TaskStatus::Failed;
                        mt.task.status_message = Some(format!("Error: {}", e.message));
                        mt.result = Some(CallToolResult::error(vec![Content::text(
                            e.message.clone(),
                        )]));
                    }
                    None => {
                        mt.task.status = TaskStatus::Cancelled;
                        mt.task.status_message = Some("Cancelled by client".to_string());
                    }
                }
                mt.task.last_updated_at = now;
            }
        });

        task
    }

    /// List all tasks (for `tasks/list`).
    pub async fn list_tasks(&self) -> Vec<Task> {
        let map = self.tasks.lock().await;
        map.values().map(|mt| mt.task.clone()).collect()
    }

    /// Get info for a specific task (for `tasks/get`).
    pub async fn get_task_info(&self, task_id: &str) -> Option<Task> {
        let map = self.tasks.lock().await;
        map.get(task_id).map(|mt| mt.task.clone())
    }

    /// Get the result of a completed task (for `tasks/result`).
    pub async fn get_task_result(&self, task_id: &str) -> Option<serde_json::Value> {
        let map = self.tasks.lock().await;
        let mt = map.get(task_id)?;
        if mt.task.status != TaskStatus::Completed {
            return None;
        }
        let result = mt.result.as_ref()?;
        serde_json::to_value(result).ok()
    }

    /// Cancel a running task.
    pub async fn cancel_task(&self, task_id: &str) -> Option<Task> {
        let map = self.tasks.lock().await;
        let mt = map.get(task_id)?;
        if mt.task.status == TaskStatus::Working {
            let _ = mt.cancel_token.send(true);
        }
        Some(mt.task.clone())
    }

    /// Clean up expired tasks (TTL-based). Call periodically.
    pub async fn cleanup_expired(&self) {
        let now = chrono::Utc::now();
        let mut map = self.tasks.lock().await;
        map.retain(|_, mt| {
            // Only clean up terminal states
            if matches!(
                mt.task.status,
                TaskStatus::Working | TaskStatus::InputRequired
            ) {
                return true;
            }
            // Check TTL
            if let Some(ttl_ms) = mt.task.ttl {
                if let Ok(updated) = mt
                    .task
                    .last_updated_at
                    .parse::<chrono::DateTime<chrono::Utc>>()
                {
                    let age_ms = (now - updated).num_milliseconds();
                    return age_ms < ttl_ms as i64;
                }
            }
            true // Keep if we can't determine age
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_is_taskable() {
        assert!(is_taskable("csr_reflect_on_past"));
        assert!(is_taskable("search_by_recency"));
        assert!(!is_taskable("store_reflection"));
        assert!(!is_taskable("csr_quick_check"));
    }

    #[tokio::test]
    async fn test_enqueue_and_complete() {
        let mgr = TaskManager::new();

        // Enqueue a task that completes immediately
        let task = mgr
            .enqueue("csr_reflect_on_past", async {
                Ok(CallToolResult::success(vec![Content::text("found it")]))
            })
            .await;

        assert_eq!(task.status, TaskStatus::Working);
        assert!(task.task_id.starts_with("csr-task-"));

        // Wait for completion
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let info = mgr.get_task_info(&task.task_id).await.unwrap();
        assert_eq!(info.status, TaskStatus::Completed);

        let result = mgr.get_task_result(&task.task_id).await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_enqueue_and_fail() {
        let mgr = TaskManager::new();

        let task = mgr
            .enqueue("csr_reflect_on_past", async {
                Err(rmcp::ErrorData::internal_error("search failed", None))
            })
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let info = mgr.get_task_info(&task.task_id).await.unwrap();
        assert_eq!(info.status, TaskStatus::Failed);
        assert!(info.status_message.unwrap().contains("search failed"));
    }

    #[tokio::test]
    async fn test_cancel_task() {
        let mgr = TaskManager::new();

        // Enqueue a slow task
        let task = mgr
            .enqueue("csr_reflect_on_past", async {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                Ok(CallToolResult::success(vec![Content::text("done")]))
            })
            .await;

        // Cancel it
        let cancelled = mgr.cancel_task(&task.task_id).await.unwrap();
        assert_eq!(cancelled.status, TaskStatus::Working); // Still working at cancel time

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let info = mgr.get_task_info(&task.task_id).await.unwrap();
        assert_eq!(info.status, TaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn test_list_tasks() {
        let mgr = TaskManager::new();

        mgr.enqueue("csr_reflect_on_past", async {
            Ok(CallToolResult::success(vec![Content::text("a")]))
        })
        .await;

        mgr.enqueue("search_by_recency", async {
            Ok(CallToolResult::success(vec![Content::text("b")]))
        })
        .await;

        let tasks = mgr.list_tasks().await;
        assert_eq!(tasks.len(), 2);
    }

    #[tokio::test]
    async fn test_unique_ids() {
        let mgr = TaskManager::new();
        let t1 = mgr
            .enqueue("csr_reflect_on_past", async {
                Ok(CallToolResult::success(vec![]))
            })
            .await;
        let t2 = mgr
            .enqueue("csr_reflect_on_past", async {
                Ok(CallToolResult::success(vec![]))
            })
            .await;
        assert_ne!(t1.task_id, t2.task_id);
    }

    #[tokio::test]
    async fn test_get_nonexistent_task() {
        let mgr = TaskManager::new();
        assert!(mgr.get_task_info("no-such-task").await.is_none());
        assert!(mgr.get_task_result("no-such-task").await.is_none());
    }
}
