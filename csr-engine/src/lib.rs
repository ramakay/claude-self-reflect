pub mod api;
pub mod binary_identity;
pub mod claude_sessions;
pub mod codemap;
pub mod companion;
pub mod daemon;
pub mod daemon_history;
pub mod daemon_log;
pub mod daemon_start;
pub mod dream;
pub mod embeddings;
pub mod engine;
pub mod episodes;
pub mod eval;
/// Test programs written without a write handle a parallel fork could copy (ETXTBSY).
#[cfg(test)]
#[path = "../tests/support/exec_script.rs"]
pub(crate) mod exec_script;
#[cfg(test)]
mod exec_script_tests;
pub mod extraction;
pub mod format;
pub mod governor;
pub mod health;
pub mod hooks;
pub mod import;
pub mod index_gen;
pub mod injection;
pub mod journal;
pub mod ledger;
pub mod maintenance;
pub mod malloc_role;
pub mod mcp;
pub mod mirror;
pub mod mirror_cli;
pub mod narrative;
pub mod private_files;
pub mod provenance;
pub mod reaction_shadow;
pub mod redact;
pub mod runtime;
pub mod search;
pub mod setup;
pub mod shell;
pub mod stale_report;
pub mod status;
pub mod storage;
pub mod story_map;
pub mod summarizer;
pub mod system_export;
pub mod telemetry;
pub mod temporal;
pub mod transcript;
pub mod uninstall;

pub(crate) mod process;
