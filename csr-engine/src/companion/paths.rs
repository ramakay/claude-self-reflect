use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    pub keep: PathBuf,
    pub db: PathBuf,
    pub projects: PathBuf,
}
impl Paths {
    pub fn new(home: PathBuf) -> Self {
        Self {
            keep: home.join(".claude-self-reflect/csr-companion"),
            db: home.join(".claude-self-reflect/csr-engine.db"),
            projects: home.join(".claude/projects"),
            home,
        }
    }
    pub fn from_env() -> Self {
        Self::new(std::env::var_os("HOME").unwrap_or_default().into())
    }
    pub fn kept_file(&self, folder: &str, sid: &str) -> Option<PathBuf> {
        valid_id(sid).then(|| self.keep.join(folder).join(format!("{sid}.json")))
    }
    pub fn transcript(&self, sid: &str) -> Option<PathBuf> {
        if !valid_id(sid) {
            return None;
        }
        folders(&self.projects)
            .into_iter()
            .map(|p| p.join(format!("{sid}.jsonl")))
            .find(|p| p.is_file())
    }
    pub fn project_folder(&self, cwd: &str) -> PathBuf {
        self.projects.join(
            cwd.chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect::<String>(),
        )
    }
}
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}
pub fn folders(path: &Path) -> Vec<PathBuf> {
    let mut out: Vec<_> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}
pub fn read_json(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}
pub fn epoch(text: &str) -> Option<f64> {
    let text = text.trim().replacen(' ', "T", 1);
    let parsed = chrono::DateTime::parse_from_rfc3339(&text)
        .ok()
        .map(|v| v.timestamp_micros() as f64 / 1e6);
    parsed.or_else(|| {
        chrono::NaiveDateTime::parse_from_str(&text, "%Y-%m-%dT%H:%M:%S%.f")
            .ok()
            .map(|v| v.and_utc().timestamp_micros() as f64 / 1e6)
    })
}
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
pub fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
pub fn csr_project(path: &str) -> Option<&str> {
    let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
    parts
        .windows(2)
        .find(|p| p[0] == "projects")
        .map(|p| p[1])
        .or_else(|| parts.last().copied())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_kept_file_refuses_a_path() {
        let p = Paths::new("/fixture".into());
        assert!(p.kept_file("panes", "../x").is_none());
    }
    #[test]
    fn test_project_folder() {
        let p = Paths::new("/fixture".into());
        assert_eq!(
            p.project_folder("/a_b/c.d"),
            PathBuf::from("/fixture/.claude/projects/-a-b-c-d")
        );
    }
    #[test]
    fn test_epoch() {
        assert_eq!(
            epoch("2026-10-08 22:33:57.100000+00:00"),
            Some(1791498837.1)
        );
        assert_eq!(epoch("odd"), None);
    }
}
