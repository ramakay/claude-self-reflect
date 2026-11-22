//! The `csr-engine episodes` subcommands, as clap sees them. Lives here so `main.rs` stays small.

use std::path::PathBuf;

#[derive(clap::Subcommand, Debug)]
pub(crate) enum EpisodesAction {
    /// Parse mirrored transcripts into episodes and session cards (no model).
    Build {
        /// Mirror transcripts root (`<rel>.jsonl.zst` or plain `.jsonl`).
        #[arg(long)]
        mirror_root: PathBuf,
        /// Only files and episodes newer than this many days.
        #[arg(long)]
        since_days: Option<i64>,
        /// Also write the stats JSON here.
        #[arg(long)]
        stats_out: Option<PathBuf>,
    },
    /// Write episodes as `embed sample` rows.
    Export {
        #[arg(long)]
        since_days: Option<i64>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Add expect_sessions to a probe file from the chunks table.
    Truth {
        #[arg(long)]
        probes: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}
