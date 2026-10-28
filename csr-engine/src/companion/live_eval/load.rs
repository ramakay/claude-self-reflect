//! The one place the live eval touches the disk.
use super::eval::evaluate;
use super::samples::{parse, Sample};
use super::spec::Trial;
use super::words::{card, unreadable, Card};
use chrono::Local;
use std::path::{Path, PathBuf};

/// A trial stays on the card this long after its planned end.
pub const SHOWN_AFTER_END: f64 = 12. * 3600.;
/// The most of a samples file that is read.
const MAX_SAMPLES_BYTES: u64 = 8 << 20;

pub enum Loaded {
    /// No trial is running or lately ended: no card.
    Hidden,
    /// The trial and its samples, or why they could not be read.
    Trial {
        trial: Box<Trial>,
        samples: Result<Vec<Sample>, String>,
    },
    /// A spec that could not be read, lately written: why.
    Unreadable(String),
}

fn mtime(path: &Path) -> Option<f64> {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
}

fn read_samples(trial: &Trial, dir: &Path) -> Result<Vec<Sample>, String> {
    let source = PathBuf::from(&trial.source);
    let path = if source.is_absolute() {
        source
    } else {
        dir.join(source)
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let meta = path.metadata().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{name} not found"),
        _ => format!("{name}: {e}"),
    })?;
    if meta.len() > MAX_SAMPLES_BYTES {
        return Err(format!("{name} is over {} MB", MAX_SAMPLES_BYTES >> 20));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("{name}: {e}"))?;
    parse(&String::from_utf8_lossy(&bytes), &trial.time_field)
}

/// The trial to show at `now`: of the specs in `dir` whose window (the start to twelve hours past
/// the planned end) holds `now`, the newest to have started.
pub fn load(dir: &Path, now: f64) -> Loaded {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Loaded::Hidden;
    };
    let mut best: Option<Trial> = None;
    let mut broken: Option<String> = None;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| Trial::from_json(&text));
        match parsed {
            Ok(trial) => {
                let shown = trial.start <= now && now <= trial.end + SHOWN_AFTER_END;
                if shown && best.as_ref().is_none_or(|b| trial.start >= b.start) {
                    best = Some(trial);
                }
            }
            // A spec just written wrong is worth saying; one long forgotten is not.
            Err(why) => {
                if mtime(&path).is_some_and(|m| (now - m).abs() <= SHOWN_AFTER_END) {
                    broken = Some(format!("{name}: {why}"));
                }
            }
        }
    }
    match (best, broken) {
        (Some(trial), _) => {
            let samples = read_samples(&trial, dir);
            Loaded::Trial {
                trial: Box::new(trial),
                samples,
            }
        }
        (None, Some(why)) => Loaded::Unreadable(why),
        (None, None) => Loaded::Hidden,
    }
}

/// The card for what was loaded, as of `now`; `None` when no trial is on show.
pub fn card_at(loaded: &Loaded, now: f64) -> Option<Card> {
    match loaded {
        Loaded::Hidden => None,
        Loaded::Unreadable(why) => Some(unreadable(None, why, now, &Local)),
        Loaded::Trial {
            trial,
            samples: Ok(samples),
        } => Some(card(&evaluate(trial, samples, now), &Local)),
        Loaded::Trial {
            trial,
            samples: Err(why),
        } => Some(unreadable(Some(&**trial), why, now, &Local)),
    }
}
