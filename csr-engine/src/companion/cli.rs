use clap::{Args, Subcommand};
use std::{io::Read, path::PathBuf};

#[derive(Args, Debug)]
#[command(version)]
pub struct CompanionArgs {
    #[command(subcommand)]
    pub command: CompanionCmd,
}

#[derive(Subcommand, Debug)]
pub enum CompanionCmd {
    Pane(Box<PaneArgs>),
    Private {
        #[command(subcommand)]
        command: PrivateCmd,
    },
}

#[derive(Args, Debug)]
#[command(group(clap::ArgGroup::new("mode").required(true).args(["watch", "frame", "json", "png", "note", "brief", "story_news", "lost", "keep", "coach", "compact_plan", "compact_usual"])))]
pub struct PaneArgs {
    #[arg(long)]
    pub repo: PathBuf,
    #[arg(long, default_value_t = 120)]
    pub limit: usize,
    #[arg(long)]
    pub sid: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub watch: bool,
    #[arg(long, hide = true)]
    pub frame: bool,
    #[arg(long, hide = true, default_value_t = 60)]
    pub cols: usize,
    #[arg(long, hide = true, default_value_t = 30)]
    pub rows: usize,
    #[arg(long, hide = true, default_value = "main")]
    pub page: String,
    #[arg(long, hide = true)]
    pub now: Option<f64>,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub png: Option<PathBuf>,
    #[arg(long)]
    pub note: Option<PathBuf>,
    #[arg(long)]
    pub brief: bool,
    #[arg(long)]
    pub story_news: bool,
    #[arg(long)]
    pub lost: bool,
    #[arg(long)]
    pub keep: Option<String>,
    #[arg(long)]
    pub coach: bool,
    #[arg(long)]
    pub compact_plan: bool,
    #[arg(long)]
    pub compact_usual: bool,
    #[arg(long)]
    pub touch: Option<String>,
    #[arg(long)]
    pub ctx: Option<usize>,
}

#[derive(Subcommand, Debug)]
pub enum PrivateCmd {
    Write {
        path: PathBuf,
        #[arg(long, conflicts_with = "redact_json")]
        redact: bool,
        #[arg(long)]
        redact_json: bool,
    },
    Tighten,
}

pub fn run(args: &CompanionArgs) -> i32 {
    let result = match &args.command {
        CompanionCmd::Pane(_) => {
            eprintln!("companion pane: not yet ported");
            return 2;
        }
        CompanionCmd::Private { command } => run_private(command),
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("companion: {error:#}");
            1
        }
    }
}

fn run_private(command: &PrivateCmd) -> anyhow::Result<()> {
    let root = super::private::default_root();
    match command {
        PrivateCmd::Tighten => super::private::tighten(&root)?,
        PrivateCmd::Write {
            path,
            redact,
            redact_json,
        } => {
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes)?;
            let mut text = String::from_utf8_lossy(&bytes).into_owned();
            if *redact_json {
                let mut value = serde_json::from_str(&text).map_err(|error| {
                    anyhow::anyhow!("--redact-json was given text that is not JSON: {error}")
                })?;
                super::private::redact_value(&mut value);
                text = serde_json::to_string(&value)?;
            } else if *redact {
                text = crate::redact::redact_private(&text).into_owned();
            }
            super::private::write_atomic(path, &text, &root)?;
        }
    }
    Ok(())
}
