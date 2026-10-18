//! `csr-engine symbol change`: did a commit author a change to a symbol, and
//! if it is a merge that did not, which commit did?
//!
//! The answer is computed with the receipt module's own machinery
//! ([`super::receipt::ReceiptResolver`]): the same span-level
//! [`SymbolState`](super::receipt::SymbolState), the same symbol naming as the
//! witness ledger, and the same merge rule the live dream receipt resolver
//! applies ("a merge whose first-parent diff leaves the span unchanged did not
//! author the change"). It reads git only; it never opens the database.
//!
//! The command prints exactly one JSON object with stable keys:
//! `oid`, `merge`, `authored`, `author`, `author_committed_at`. Later items may
//! ADD keys; none of these are renamed or removed.

use std::path::Path;

use codewitness::Auditor;
use serde_json::{json, Value};

use super::receipt::{commit_time_local, ReceiptResolver, Subject, SymbolState};
use crate::import::backfill::{git_at, relpath_in_repo};
use crate::process::CommandDeadline;

/// One `symbol change` question.
pub struct ChangeRequest<'a> {
    /// Repository root.
    pub repo: &'a str,
    /// File, relative to `repo` (an absolute path under `repo` is accepted).
    pub file: &'a str,
    /// Ledger symbol; `None` = the whole file.
    pub symbol: Option<&'a str>,
    /// The commit asked about (any rev git resolves to a commit).
    pub oid: &'a str,
    /// The witness's `at_oid`, needed to resolve the author of a merge.
    pub from: Option<&'a str>,
}

/// Full hex oid of the commit `rev` names, or `None`.
fn resolve_commit(repo: &str, rev: &str) -> Option<String> {
    if rev.is_empty() || rev.starts_with('-') {
        return None;
    }
    let out = git_at(repo)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{rev}^{{commit}}"))
        .output_with_deadline(crate::process::GIT_TIMEOUT)
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let oid = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!oid.is_empty()).then_some(oid)
}

/// The path of `file` relative to `repo`, refusing anything that could leave
/// the repository.
fn relative_file(repo: &str, file: &str) -> Option<String> {
    let rel = if Path::new(file).is_absolute() {
        relpath_in_repo(repo, file)?
    } else {
        file.to_string()
    };
    let escapes = Path::new(&rel)
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)));
    (!rel.is_empty() && !escapes).then_some(rel)
}

/// Answer one `symbol change` question as the JSON object described in the
/// module doc, or an error message for an unreadable repository / commit.
pub fn symbol_change(req: &ChangeRequest) -> Result<Value, String> {
    if !Path::new(req.repo).is_dir() {
        return Err(format!("repo is not a directory: {}", req.repo));
    }
    let relpath = relative_file(req.repo, req.file)
        .ok_or_else(|| format!("file is not a path inside the repo: {}", req.file))?;
    let oid = resolve_commit(req.repo, req.oid)
        .ok_or_else(|| format!("not a commit in {}: {}", req.repo, req.oid))?;
    let from = match req.from {
        Some(rev) => Some(
            resolve_commit(req.repo, rev)
                .ok_or_else(|| format!("--from is not a commit in {}: {rev}", req.repo))?,
        ),
        None => None,
    };
    let auditor =
        Auditor::discover(req.repo).map_err(|e| format!("cannot open repo {}: {e}", req.repo))?;
    let absolute = Path::new(req.repo).join(&relpath);
    let absolute = absolute.to_string_lossy();
    let subject = Subject {
        auditor: &auditor,
        repo_root: req.repo,
        file: &absolute,
        relpath: &relpath,
        project: "",
        symbol: req.symbol,
    };
    let mut resolver = ReceiptResolver::new();
    let parents = resolver
        .parents_of(req.repo, &oid)
        .ok_or_else(|| format!("cannot read the parents of {oid}"))?;
    let merge = parents.len() >= 2;
    let authored = resolver.changed_vs_first_parent(&subject, &oid);
    let author = match authored {
        Some(true) => Some(oid.clone()),
        Some(false) if merge => from
            .as_deref()
            .and_then(|from| merge_author(&mut resolver, &subject, from, &oid)),
        _ => None,
    };
    let author_committed_at = author
        .as_deref()
        .and_then(|a| commit_time_local(&auditor, a));
    Ok(json!({
        "oid": oid,
        "merge": merge,
        "authored": authored,
        "author": author,
        "author_committed_at": author_committed_at,
    }))
}

/// The commit that authored the change merge `merge` carries, judged from the
/// witness at `from`. The change is "the span left the witness's state"; the
/// verdict kind is read off the merge's own state: gone (`Absent`) or other
/// content (`Present` and different). A merge that lands on the witness's own
/// state (a reinstatement, whose real search origin is not `from`), unknown
/// states, and every failed search answer `None`.
fn merge_author(
    resolver: &mut ReceiptResolver,
    subject: &Subject,
    from: &str,
    merge: &str,
) -> Option<String> {
    let base = resolver.state_at(subject, from);
    let now = resolver.state_at(subject, merge);
    if base == SymbolState::Unknown || now == SymbolState::Unknown || now == base {
        return None;
    }
    let pre = |s: &SymbolState| *s == base;
    let gone = now == SymbolState::Absent;
    let pred = |s: &SymbolState| {
        if gone {
            *s == SymbolState::Absent
        } else {
            matches!(s, SymbolState::Present(_)) && *s != base
        }
    };
    resolver
        .authoring_commit(subject, from, merge, &pre, &pred, 0)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = git_at(&dir.to_string_lossy())
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn commit(dir: &Path, msg: &str) -> String {
        run(dir, &["add", "-A"]);
        run(dir, &["commit", "-q", "-m", msg]);
        run(dir, &["rev-parse", "HEAD"])
    }

    /// main: c0 -> c1 (`foo` 1 -> 2) -> m (merges `side`, which only touched
    /// other.txt). Returns (dir, s1, c1, m).
    fn fixture() -> (tempfile::TempDir, String, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        run(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("lib.rs"), "fn foo() {\n    1\n}\n").unwrap();
        std::fs::write(dir.join("other.txt"), "0\n").unwrap();
        commit(dir, "c0");
        run(dir, &["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.join("other.txt"), "side\n").unwrap();
        let s1 = commit(dir, "s1");
        run(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("lib.rs"), "fn foo() {\n    2\n}\n").unwrap();
        let c1 = commit(dir, "c1");
        run(dir, &["merge", "--no-ff", "-q", "-m", "merge", "side"]);
        let m = run(dir, &["rev-parse", "HEAD"]);
        (tmp, s1, c1, m)
    }

    fn ask(dir: &Path, oid: &str, from: Option<&str>) -> Result<Value, String> {
        symbol_change(&ChangeRequest {
            repo: &dir.to_string_lossy(),
            file: "lib.rs",
            symbol: Some("foo"),
            oid,
            from,
        })
    }

    #[test]
    fn a_merge_that_left_the_symbol_unchanged_resolves_to_its_author() {
        let (tmp, s1, c1, m) = fixture();
        let answer = ask(tmp.path(), &m[..8], Some(&s1[..8])).unwrap();
        assert_eq!(answer["oid"], m.as_str());
        assert_eq!(answer["merge"], true);
        assert_eq!(answer["authored"], false);
        assert_eq!(answer["author"], c1.as_str());
        let at = answer["author_committed_at"].as_str().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(at).is_ok(), "{at}");
    }

    #[test]
    fn without_from_a_merge_has_no_author() {
        let (tmp, _s1, _c1, m) = fixture();
        let answer = ask(tmp.path(), &m, None).unwrap();
        assert_eq!(answer["merge"], true);
        assert_eq!(answer["authored"], false);
        assert!(answer["author"].is_null());
        assert!(answer["author_committed_at"].is_null());
    }

    #[test]
    fn the_authoring_commit_authored_itself() {
        let (tmp, s1, c1, _m) = fixture();
        let answer = ask(tmp.path(), &c1, Some(&s1)).unwrap();
        assert_eq!(answer["merge"], false);
        assert_eq!(answer["authored"], true);
        assert_eq!(answer["author"], c1.as_str());
        assert!(answer["author_committed_at"].is_string());
    }

    #[test]
    fn a_commit_that_did_not_touch_the_symbol_is_not_authored() {
        let (tmp, s1, _c1, _m) = fixture();
        let answer = ask(tmp.path(), &s1, None).unwrap();
        assert_eq!(answer["merge"], false);
        assert_eq!(answer["authored"], false);
        assert!(answer["author"].is_null());
    }

    #[test]
    fn a_root_commit_authored_a_present_symbol() {
        let (tmp, ..) = fixture();
        let root = run(tmp.path(), &["rev-list", "--max-parents=0", "HEAD"]);
        let answer = ask(tmp.path(), &root, None).unwrap();
        assert_eq!(answer["authored"], true);
        assert_eq!(answer["author"], root.as_str());
    }

    #[test]
    fn an_unparseable_file_reads_as_null_not_false() {
        let (tmp, _s1, _c1, _m) = fixture();
        std::fs::write(tmp.path().join("lib.rs"), "fn foo( {{{ ]\n").unwrap();
        let bad = commit(tmp.path(), "broken");
        let answer = ask(tmp.path(), &bad, None).unwrap();
        assert!(answer["authored"].is_null(), "{answer}");
        assert!(answer["author"].is_null());
    }

    #[test]
    fn an_unreadable_commit_or_path_is_an_error() {
        let (tmp, ..) = fixture();
        assert!(ask(tmp.path(), "deadbeef", None).is_err());
        assert!(ask(tmp.path(), "--all", None).is_err());
        let escape = symbol_change(&ChangeRequest {
            repo: &tmp.path().to_string_lossy(),
            file: "../lib.rs",
            symbol: None,
            oid: "HEAD",
            from: None,
        });
        assert!(escape.is_err());
        assert!(ask(Path::new("/nonexistent/csr-no-repo"), "HEAD", None).is_err());
    }
}
