//! The `csr-engine assertions` subcommands, as clap sees them. Lives here so `main.rs` stays small.

use std::path::PathBuf;

#[derive(clap::Subcommand, Debug)]
pub(crate) enum AssertionsAction {
    /// Rebuild every assertion from the decisions file and the ledgers (no model).
    Build {
        /// Frozen user-decisions JSON (a `decisions` array).
        #[arg(long)]
        decisions: PathBuf,
        /// Also write the report JSON here.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Print assertion rows, newest first.
    List {
        /// user_decision, csr_resolve, witness or task.
        #[arg(long)]
        source: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}
