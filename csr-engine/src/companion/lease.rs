use super::{num, paths::read_json, private};
use clap::{Args, ValueEnum};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

const LEASE_MS: f64 = 15. * 60. * 1000.;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Step {
    Take,
    Renew,
    Release,
}

#[derive(Args, Debug)]
pub struct LeaseArgs {
    pub step: Step,
    #[arg(long)]
    pub folder: PathBuf,
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub root: String,
    #[arg(long)]
    pub rank: i64,
    #[arg(long)]
    pub now: f64,
    #[arg(long)]
    pub busy: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Lease {
    pub root: String,
    pub rank: i64,
    pub at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LeaseOutput {
    pub held: bool,
    pub lease: Value,
}

pub fn decide(args: &LeaseArgs, held: &Value) -> (bool, Option<Value>) {
    let mine = held.is_object() && held["root"] == args.root;
    match args.step {
        Step::Take => {
            let fresh = held["at"]
                .as_f64()
                .is_some_and(|at| args.now.trunc() - at < LEASE_MS);
            if held.as_object().is_some_and(|v| !v.is_empty())
                && !mine
                && fresh
                && (num(&held["rank"]) >= args.rank as f64 || super::truthy(&held["busy"]))
            {
                return (false, None);
            }
            let lease = Lease {
                root: args.root.clone(),
                rank: args.rank,
                at: args.now as i64,
                busy: args.busy.then_some(true),
            };
            (true, Some(serde_json::to_value(lease).unwrap()))
        }
        Step::Renew if mine => {
            let mut lease = held.clone();
            lease["at"] = Value::from(args.now as i64);
            (true, Some(lease))
        }
        Step::Release if mine && super::truthy(&held["busy"]) => {
            let mut lease = held.clone();
            let object = lease.as_object_mut().unwrap();
            object.retain(|key, _| matches!(key.as_str(), "root" | "rank" | "at"));
            object.entry("rank").or_insert(args.rank.into());
            object.entry("at").or_insert(0.into());
            (true, Some(lease))
        }
        _ => (mine, None),
    }
}

pub fn step_under_lock(args: &LeaseArgs) -> anyhow::Result<LeaseOutput> {
    anyhow::ensure!(super::paths::valid_id(&args.session), "invalid session id");
    anyhow::ensure!(args.now.is_finite(), "invalid lease time");
    let root = private::default_root();
    let path = args.folder.join(format!("{}.json", args.session));
    let lock_path = args.folder.join(format!("{}.lease.lock", args.session));
    let lock = private::open_private(&lock_path, true, &root)?;
    lock.lock_exclusive()?;
    let held = read_json(&path);
    if let Ok(pause) = std::env::var("CSR_LEASE_PAUSE_MS") {
        std::thread::sleep(std::time::Duration::from_millis(pause.parse()?));
    }
    let (mine, lease) = decide(args, &held);
    if let Some(lease) = &lease {
        private::write_atomic(&path, &serde_json::to_string(lease)?, &root)?;
    }
    Ok(LeaseOutput {
        held: mine,
        lease: lease.unwrap_or(held),
    })
}
