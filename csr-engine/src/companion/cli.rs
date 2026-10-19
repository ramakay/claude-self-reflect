use clap::{Args, Subcommand};
use std::{io::Read, path::PathBuf};

#[derive(Args, Debug)]
#[command(version, display_name = "csr-engine-companion")]
pub struct CompanionArgs {
    #[command(subcommand)]
    pub command: CompanionCmd,
}

#[derive(Subcommand, Debug)]
pub enum CompanionCmd {
    Pane(Box<PaneArgs>),
    Lease(super::lease::LeaseArgs),
    Private {
        #[command(subcommand)]
        command: PrivateCmd,
    },
}

#[derive(Args, Debug, Clone)]
#[command(group(
    clap::ArgGroup::new("mode").required(true).args([
        "watch", "frame", "json", "png", "note", "brief", "story_news",
        "lost", "keep", "coach", "compact_plan", "compact_usual"
    ])
))]
pub struct PaneArgs {
    #[arg(long, default_value = ".")]
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
    #[arg(
        long, hide = true, default_value_t = 1, requires = "frame",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub repeat: u32,
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
        CompanionCmd::Pane(args) => {
            if args.keep.is_some()
                || args.note.is_some()
                || args.json
                || args.lost
                || args.compact_plan
                || args.compact_usual
                || args.brief
                || args.coach
                || args.story_news
            {
                return match run_hook(args) {
                    Ok(()) => 0,
                    Err(error) => {
                        eprintln!("companion: {error:#}");
                        1
                    }
                };
            }
            if !(args.watch || args.frame || args.png.is_some())
                || !matches!(args.page.as_str(), "main" | "log")
            {
                eprintln!("companion pane: not yet ported");
                return 2;
            }
            let paths = super::paths::Paths::from_env();
            if args.watch {
                let mut args = args.as_ref().clone();
                if let Some(agent) = args.agent.as_deref() {
                    args.sid = super::send::attached_session(&paths, agent).or(args.sid);
                }
                super::watch::watch(paths, &args)
            } else if args.png.is_some() {
                super::png::export(paths, args)
            } else {
                let now = args
                    .now
                    .or_else(|| {
                        std::env::var("CSR_COMPANION_NOW")
                            .ok()
                            .and_then(|s| s.parse().ok())
                    })
                    .unwrap_or_else(super::paths::now);
                let mut pane = super::pane::Pane::new(paths, args);
                let mut times = Vec::with_capacity(args.repeat as usize);
                let mut first = Vec::new();
                for index in 0..args.repeat {
                    let start = std::time::Instant::now();
                    let frame = pane.frame(args.cols, args.rows, now);
                    times.push(start.elapsed().as_secs_f64() * 1000.);
                    if index == 0 {
                        first = frame;
                    }
                }
                println!("{}", first.join("\n"));
                if args.repeat > 1 {
                    let first = times.remove(0);
                    times.sort_by(f64::total_cmp);
                    let middle = times.len() / 2;
                    let median = if times.len() % 2 == 0 {
                        (times[middle - 1] + times[middle]) / 2.
                    } else {
                        times[middle]
                    };
                    eprintln!("first frame: {first:.3} ms; subsequent median: {median:.3} ms");
                }
                Ok(())
            }
        }
        CompanionCmd::Lease(args) => super::lease::step_under_lock(args).and_then(|out| {
            println!("{}", serde_json::to_string(&out)?);
            Ok(())
        }),
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

fn run_hook(args: &PaneArgs) -> anyhow::Result<()> {
    let paths = super::paths::Paths::from_env();
    let sid = args.sid.as_deref().unwrap_or("");
    let now = args
        .now
        .or_else(|| {
            std::env::var("CSR_COMPANION_NOW")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or_else(super::paths::now);
    if args.coach {
        println!(
            "{}",
            serde_json::to_string(&super::coach::coach_terms(&paths, now))?
        );
    } else if args.story_news {
        let db = super::db::Db::open(&paths).ok();
        let news = if let Some(db) = &db {
            let root = db.pane_root(&args.repo);
            super::story::story_news_in(db, &paths, root.as_deref(), sid, now)
        } else {
            super::story::News::Absent { story: false }
        };
        println!("{}", serde_json::to_string(&news)?);
    } else if args.brief {
        if !sid.is_empty() {
            let db = super::db::Db::open(&paths).ok();
            let root = super::paths::git(&args.repo, &["rev-parse", "--show-toplevel"])
                .map(std::path::PathBuf::from)
                .or_else(|| db.as_ref().and_then(|db| db.pane_root(&args.repo)));
            if let Some(root) = root {
                let db = db.ok_or_else(|| anyhow::anyhow!("could not read csr-engine"))?;
                println!("{}", super::brief::brief(&db, &paths, &root, sid, now));
            }
        }
    } else if args.lost {
        println!(
            "{}",
            serde_json::to_string(&super::lost::lost_json(&paths, sid))?
        );
    } else if args.compact_plan {
        println!(
            "{}",
            serde_json::to_string(&super::compact::compact_plan_json(
                &paths,
                sid,
                args.ctx.unwrap_or(0),
            ))?
        );
    } else if args.compact_usual {
        println!(
            "{}",
            serde_json::to_string(&super::compact::keep_compact_usual(&paths, now))?
        );
    } else if let Some(record) = &args.keep {
        let mut record: serde_json::Value = serde_json::from_str(record)?;
        if ["dream", "look", "chapter", "ysk", "compact"].contains(&super::string(&record["kind"]))
        {
            let root = super::paths::git(&args.repo, &["rev-parse", "--show-toplevel"])
                .map(std::path::PathBuf::from)
                .or_else(|| super::db::Db::open(&paths).ok()?.pane_root(&args.repo));
            record["repo"] = serde_json::to_value(root)?;
        }
        super::dreams::keep(&paths, &record)?;
        println!("{{}}");
    } else if let Some(path) = &args.note {
        let path = super::paths::realpath(path);
        let root = path
            .parent()
            .and_then(|p| super::paths::git(p, &["rev-parse", "--show-toplevel"]));
        let touched = args.touch.as_deref() == Some("-");
        let mut output = super::note::NotesOutput::default();
        if let Some(root) = root {
            let db = super::db::Db::open(&paths)?;
            let touch = if touched {
                let mut bytes = Vec::new();
                std::io::stdin()
                    .take(super::note::TOUCH_MAX + 1)
                    .read_to_end(&mut bytes)?;
                let raw = if bytes.len() as u64 > super::note::TOUCH_MAX {
                    serde_json::Value::Null
                } else {
                    serde_json::from_slice(&bytes).unwrap_or_default()
                };
                Some(super::note::touch_of(&raw, &path))
            } else {
                None
            };
            output = super::note::notes_for(
                &db,
                &paths,
                std::path::Path::new(&root),
                &path,
                sid,
                None,
                touch.as_ref(),
            );
        }
        if touched {
            println!("{}", serde_json::to_string(&output)?);
        } else {
            println!("{}", serde_json::to_string(&output.notes)?);
        }
    } else if args.json {
        println!(
            "{}",
            serde_json::to_string(&super::hook_json::pane_json(
                &paths, &args.repo, sid, args.limit, now,
            ))?
        );
    }
    Ok(())
}
