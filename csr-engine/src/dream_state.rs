use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

const MAX_DREAM_AGE: Duration = Duration::minutes(15);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DreamMarker {
    pub started_at: DateTime<Utc>,
    pub engine: String,
    pub pid: u32,
}

pub fn marker_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude-self-reflect").join("dreaming.json"))
}

pub fn write_marker(engine: &str) {
    #[cfg(test)]
    {
        let _ = engine;
        let _ = marker_path();
    }
    #[cfg(not(test))]
    if let Some(path) = marker_path() {
        write_marker_at(&path, engine, Utc::now());
    }
}

pub fn clear_marker() {
    #[cfg(not(test))]
    if let Some(path) = marker_path() {
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::debug!(path = %path.display(), "cleared dream marker"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "failed to clear dream marker")
            }
        }
    }
}

pub fn read_active_marker(now: DateTime<Utc>) -> Option<DreamMarker> {
    #[cfg(test)]
    {
        let _ = now;
        None
    }
    #[cfg(not(test))]
    {
        read_active_marker_at(&marker_path()?, now)
    }
}

fn write_marker_at(path: &Path, engine: &str, started_at: DateTime<Utc>) {
    let marker = DreamMarker {
        started_at,
        engine: engine.to_string(),
        pid: std::process::id(),
    };
    let Some(parent) = path.parent() else {
        tracing::warn!(path = %path.display(), "dream marker path has no parent");
        return;
    };
    if let Err(error) = std::fs::create_dir_all(parent) {
        tracing::warn!(path = %parent.display(), %error, "failed to create dream marker directory");
        return;
    }
    let contents = match serde_json::to_vec(&marker) {
        Ok(contents) => contents,
        Err(error) => {
            tracing::warn!(%error, "failed to serialize dream marker");
            return;
        }
    };
    match std::fs::write(path, contents) {
        Ok(()) => tracing::debug!(path = %path.display(), engine, "wrote dream marker"),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "failed to write dream marker")
        }
    }
}

fn read_active_marker_at(path: &Path, now: DateTime<Utc>) -> Option<DreamMarker> {
    let contents = match std::fs::read(path) {
        Ok(contents) => contents,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(path = %path.display(), %error, "failed to read dream marker");
            }
            return None;
        }
    };
    let marker: DreamMarker = match serde_json::from_slice(&contents) {
        Ok(marker) => marker,
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "invalid dream marker");
            return None;
        }
    };
    if now - marker.started_at > MAX_DREAM_AGE {
        tracing::debug!(path = %path.display(), "ignoring stale dream marker");
        return None;
    }
    Some(marker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    #[test]
    fn fresh_marker_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dreaming.json");
        let now = Utc.timestamp_opt(1_800_000_000, 0).unwrap();

        write_marker_at(&path, "forgetting", now);

        let marker = read_active_marker_at(&path, now).expect("fresh marker");
        assert_eq!(marker.started_at, now);
        assert_eq!(marker.engine, "forgetting");
        assert_eq!(marker.pid, std::process::id());
    }

    #[test]
    fn marker_older_than_maximum_age_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dreaming.json");
        let now = Utc.timestamp_opt(1_800_000_000, 0).unwrap();
        write_marker_at(&path, "forgetting", now - Duration::minutes(20));

        assert!(read_active_marker_at(&path, now).is_none());
    }

    #[test]
    fn garbage_marker_is_inactive_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dreaming.json");
        std::fs::write(&path, "not json").unwrap();
        let now = Utc.timestamp_opt(1_800_000_000, 0).unwrap();

        assert!(read_active_marker_at(&path, now).is_none());
    }
}
