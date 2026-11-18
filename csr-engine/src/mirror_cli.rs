//! The `csr-engine mirror` subcommands, as clap sees them. Lives here so `main.rs` stays small.

#[derive(clap::Subcommand, Debug)]
pub enum MirrorAction {
    /// One full pass: mirror new transcript bytes, seal quiet transcripts, flag
    /// mirrors whose source is gone. Prints files, bytes and errors.
    Sync,
    /// Totals from the state table (no directory walk).
    Status,
    /// Delete the compressed transcripts and their state rows. Prints what it
    /// would delete unless --yes is given.
    Purge {
        /// Actually delete.
        #[arg(long)]
        yes: bool,
    },
}
