//! The one place the live eval touches the disk.
use super::eval::evaluate;
use super::samples::{parse, Sample};
use super::spec::{Track, Trial};
use super::words::{card, road_card, road_line, unreadable, Card, Row};
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

/// What the card shows: the running eval and, when the road is on, the maintainer's long road.
pub struct Shown {
    pub run: Loaded,
    pub road: Loaded,
}

/// The track a spec's text names, for a spec that failed to load. A spec that parses as JSON
/// says `"track": "road"` itself; one half-written still can, so a narrow text check finds the
/// same key and value. Anything else is a run spec.
fn track_of(text: &str) -> Track {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        return if v["track"] == "road" {
            Track::Road
        } else {
            Track::Run
        };
    }
    const KEY: &str = "\"track\"";
    let mut rest = text;
    while let Some(at) = rest.find(KEY) {
        rest = &rest[at + KEY.len()..];
        let value = rest.trim_start().strip_prefix(':');
        if value.is_some_and(|v| v.trim_start().starts_with("\"road\"")) {
            return Track::Road;
        }
    }
    Track::Run
}

fn settle(best: Option<Trial>, broken: Option<String>, dir: &Path) -> Loaded {
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

/// The trials to show at `now`: of the specs in `dir` whose window (the start to twelve hours
/// past the planned end) holds `now`, the newest to have started of each track. The road track
/// is read only when `road` is on: off, a road spec is skipped, even a broken one.
pub fn load(dir: &Path, now: f64, road: bool) -> Shown {
    let hidden = || Shown {
        run: Loaded::Hidden,
        road: Loaded::Hidden,
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return hidden();
    };
    let mut best: [Option<Trial>; 2] = [None, None];
    let mut broken: [Option<String>; 2] = [None, None];
    let slot = |track: Track| usize::from(track == Track::Road);
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
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string());
        let parsed = text.clone().and_then(|text| Trial::from_json(&text));
        match parsed {
            Ok(trial) => {
                let i = slot(trial.track);
                let shown = trial.start <= now && now <= trial.end + SHOWN_AFTER_END;
                if shown && best[i].as_ref().is_none_or(|b| trial.start >= b.start) {
                    best[i] = Some(trial);
                }
            }
            // A spec just written wrong is worth saying; one long forgotten is not.
            Err(why) => {
                let i = slot(text.map_or(Track::Run, |t| track_of(&t)));
                if mtime(&path).is_some_and(|m| (now - m).abs() <= SHOWN_AFTER_END) {
                    broken[i] = Some(format!("{name}: {why}"));
                }
            }
        }
    }
    let [best_run, best_road] = best;
    let [broken_run, broken_road] = broken;
    Shown {
        run: settle(best_run, broken_run, dir),
        road: if road {
            settle(best_road, broken_road, dir)
        } else {
            Loaded::Hidden
        },
    }
}

/// The card for what was loaded, as of `now`; `None` when nothing is on show. The run trial
/// makes the card, with the road as one more line; with no run trial the road makes the card
/// itself.
pub fn card_at(shown: &Shown, now: f64) -> Option<Card> {
    match &shown.run {
        Loaded::Hidden => match &shown.road {
            Loaded::Trial {
                trial,
                samples: Ok(samples),
            } => Some(road_card(&evaluate(trial, samples, now), &Local)),
            other => unreadable_card(other, now),
        },
        run => {
            let mut card = unreadable_card(run, now).or_else(|| match run {
                Loaded::Trial {
                    trial,
                    samples: Ok(samples),
                } => Some(card(&evaluate(trial, samples, now), &Local)),
                _ => None,
            })?;
            card.road = road_row(&shown.road, now);
            Some(card)
        }
    }
}

/// The card of a spec or its data that could not be read; `None` for a good trial or none.
fn unreadable_card(loaded: &Loaded, now: f64) -> Option<Card> {
    match loaded {
        Loaded::Hidden => None,
        Loaded::Unreadable(why) => Some(unreadable(None, why, now, &Local)),
        Loaded::Trial {
            trial,
            samples: Err(why),
        } => Some(unreadable(Some(&**trial), why, now, &Local)),
        Loaded::Trial { samples: Ok(_), .. } => None,
    }
}

/// The road as the line under a run card.
fn road_row(road: &Loaded, now: f64) -> Option<Row> {
    match road {
        Loaded::Hidden => None,
        Loaded::Trial {
            trial,
            samples: Ok(samples),
        } => Some(road_line(&evaluate(trial, samples, now))),
        other => unreadable_card(other, now).map(|c| Row {
            mark: "!".into(),
            sev: "quiet".into(),
            text: format!("Long road: {}", c.line1),
        }),
    }
}
