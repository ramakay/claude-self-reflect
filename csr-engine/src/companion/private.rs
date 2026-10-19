use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub fn default_root() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join(".claude-self-reflect/csr-companion")
}

fn absolute(path: &Path) -> io::Result<PathBuf> {
    let mut out = PathBuf::new();
    for part in std::path::absolute(path)?.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => (),
            part => out.push(part),
        }
    }
    Ok(out)
}

fn refuse_link(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io::Error::from_raw_os_error(libc::ELOOP));
    }
    Ok(())
}

fn chmod(path: &Path, mode: u32, directory: bool) -> io::Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            libc::O_NOFOLLOW | libc::O_NONBLOCK | if directory { libc::O_DIRECTORY } else { 0 },
        )
        .open(path)?;
    file.set_permissions(Permissions::from_mode(mode))
}

pub fn ensure_dir(path: &Path, root: &Path) -> io::Result<()> {
    let path = absolute(path)?;
    let root = absolute(root)?;
    let inside = path.starts_with(&root);
    let first = if inside {
        root.as_path()
    } else {
        path.as_path()
    };
    if let Some(parent) = first.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(first) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    fs::set_permissions(first, Permissions::from_mode(0o700))?;
    if inside {
        let mut folder = root.clone();
        for part in path.strip_prefix(&root).unwrap().components() {
            folder.push(part);
            refuse_link(&folder)?;
            match fs::DirBuilder::new().mode(0o700).create(&folder) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error),
            }
            chmod(&folder, 0o700, true)?;
        }
        let marker = root.join(".private");
        if !marker.exists() {
            tighten(&root)?;
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .custom_flags(libc::O_NOFOLLOW)
                .mode(0o600)
                .open(marker)?
                .set_permissions(Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

pub fn tighten(root: &Path) -> io::Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    fs::set_permissions(root, Permissions::from_mode(0o700))?;
    fn walk(root: &Path) -> io::Result<()> {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let _ = chmod(
                &entry.path(),
                if kind.is_dir() { 0o700 } else { 0o600 },
                kind.is_dir(),
            );
            if kind.is_dir() {
                let _ = walk(&entry.path());
            }
        }
        Ok(())
    }
    walk(root)
}

pub fn open_private(path: &Path, append: bool, root: &Path) -> io::Result<File> {
    let path = absolute(path)?;
    ensure_dir(path.parent().unwrap(), root)?;
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(Permissions::from_mode(0o600))?;
    Ok(file)
}

pub fn append(path: &Path, text: &str, root: &Path) -> io::Result<()> {
    open_private(path, true, root)?.write_all(text.as_bytes())
}

pub fn write_atomic(path: &Path, text: &str, root: &Path) -> io::Result<()> {
    let path = absolute(path)?;
    ensure_dir(path.parent().unwrap(), root)?;
    refuse_link(&path)?;
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(name);
    let _ = fs::remove_file(&temp);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&temp)?;
        file.set_permissions(Permissions::from_mode(0o600))?;
        file.write_all(text.as_bytes())?;
        drop(file);
        fs::rename(&temp, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn credential_key(key: &str) -> bool {
    static KEY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(concat!(
            r"(?i)^[A-Za-z0-9_.-]*?(?:password|passwd|secret|token|api[_-]?key|",
            r"access[_-]?key|private[_-]?key|client[_-]?secret|auth|authorization)$"
        ))
        .unwrap()
    });
    KEY.is_match(key)
}

pub fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = crate::redact::redact_private(text).into_owned(),
        serde_json::Value::Array(values) => values.iter_mut().for_each(redact_value),
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if credential_key(key) && value.is_string() {
                    *value = serde_json::Value::String("[redacted]".into());
                } else {
                    redact_value(value);
                }
            }
        }
        _ => (),
    }
}

pub fn dumps(value: &serde_json::Value) -> String {
    let mut value = value.clone();
    redact_value(&mut value);
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }
    #[test]
    fn test_folders_are_0700_from_the_root_down_and_files_0600() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let p = root.join("carry/deep/a.json");
        write_atomic(&p, "{}", &root).unwrap();
        for p in [&root, &root.join("carry"), &root.join("carry/deep")] {
            assert_eq!(mode(p), 0o700);
        }
        assert_eq!(mode(&p), 0o600);
        assert_eq!(fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
    }
    #[test]
    fn test_append_and_open_private_create_0600_and_keep_appending() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("dreams");
        append(&p, "one\n", d.path()).unwrap();
        append(&p, "two\n", d.path()).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "one\ntwo\n");
        assert_eq!(mode(&p), 0o600);
    }
    #[test]
    fn test_a_write_to_a_link_is_refused_and_its_target_is_untouched() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("root");
        fs::create_dir(&root).unwrap();
        let target = d.path().join("target");
        fs::write(&target, "keep").unwrap();
        let p = root.join("dreams");
        symlink(&target, &p).unwrap();
        assert!(append(&p, "x", &root).is_err());
        assert!(write_atomic(&p, "x", &root).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "keep");
    }
    #[test]
    fn test_a_link_planted_at_the_temp_name_is_not_written_through() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("target");
        fs::write(&target, "keep").unwrap();
        let p = d.path().join("x");
        symlink(
            &target,
            d.path().join(format!("x.{}.tmp", std::process::id())),
        )
        .unwrap();
        write_atomic(&p, "new", d.path()).unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "keep");
        assert_eq!(fs::read_to_string(p).unwrap(), "new");
    }
    #[test]
    fn test_a_folder_that_is_a_link_below_the_root_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("root");
        fs::create_dir(&root).unwrap();
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        symlink(&out, root.join("carry")).unwrap();
        assert!(write_atomic(&root.join("carry/a"), "x", &root).is_err());
        assert_eq!(fs::read_dir(out).unwrap().count(), 0);
    }
    #[test]
    fn test_the_first_touch_also_tightens_files_nothing_here_writes_again() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("root");
        fs::create_dir_all(root.join("statusline")).unwrap();
        let p = root.join("statusline/s.json");
        fs::write(&p, "{}").unwrap();
        fs::set_permissions(&p, Permissions::from_mode(0o644)).unwrap();
        ensure_dir(&root, &root).unwrap();
        assert_eq!(mode(&p), 0o600);
    }
    #[test]
    fn test_tighten_leaves_what_a_link_points_at_alone() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("root");
        fs::create_dir(&root).unwrap();
        let p = d.path().join("exe");
        fs::write(&p, "keep").unwrap();
        fs::set_permissions(&p, Permissions::from_mode(0o755)).unwrap();
        symlink(&p, root.join("link")).unwrap();
        tighten(&root).unwrap();
        assert_eq!(mode(&p), 0o755);
    }
    #[test]
    fn test_every_string_in_json_shaped_data_but_not_its_keys() {
        let mut v = serde_json::json!({
            "password": "sk-0123456789abcdefghij",
            "n": 3,
            "xs": ["token=abc123", null],
        });
        redact_value(&mut v);
        assert_eq!(
            v,
            serde_json::json!({
                "password": "[redacted]",
                "n": 3,
                "xs": ["token=[redacted]", null]
            })
        );
    }
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }
    #[test]
    fn test_a_0644_file_and_a_0755_folder_made_earlier_are_tightened_on_the_next_write() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("root");
        fs::create_dir_all(root.join("compacting")).unwrap();
        let old = root.join("dreams.jsonl");
        let replaced = root.join("compacting/mine.json");
        for p in [&old, &replaced] {
            fs::write(p, "old\n").unwrap();
            fs::set_permissions(p, Permissions::from_mode(0o644)).unwrap();
        }
        fs::set_permissions(&root, Permissions::from_mode(0o755)).unwrap();
        append(&old, "new\n", &root).unwrap();
        write_atomic(&replaced, "new", &root).unwrap();
        assert_eq!(fs::read_to_string(&old).unwrap(), "old\nnew\n");
        for p in [&root, &root.join("compacting")] {
            assert_eq!(mode(p), 0o700);
        }
        for p in [&old, &replaced] {
            assert_eq!(mode(p), 0o600);
        }
    }
    #[test]
    fn test_a_path_outside_the_root_makes_only_its_own_folder_private() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("root");
        let parent = d.path().join("elsewhere");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, Permissions::from_mode(0o755)).unwrap();
        let out = parent.join("lease");
        let p = out.join("x.json");
        write_atomic(&p, "{}", &root).unwrap();
        assert_eq!(mode(&parent), 0o755);
        assert_eq!(mode(&out), 0o700);
        assert_eq!(mode(&p), 0o600);
    }
    #[test]
    fn test_the_first_touch_and_the_script_leave_it_alone_too() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("root");
        fs::create_dir(&root).unwrap();
        let p = d.path().join("exe");
        fs::write(&p, "keep").unwrap();
        fs::set_permissions(&p, Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&p, root.join("link")).unwrap();
        ensure_dir(&root, &root).unwrap();
        tighten(&root).unwrap();
        assert_eq!(mode(&p), 0o755);
    }
    #[test]
    fn test_the_root_can_be_a_link_to_another_disk() {
        let d = tempfile::tempdir().unwrap();
        let actual = d.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let root = d.path().join("root");
        std::os::unix::fs::symlink(&actual, &root).unwrap();
        write_atomic(&root.join("carry/s.json"), "{}", &root).unwrap();
        assert_eq!(mode(&actual), 0o700);
        assert_eq!(mode(&actual.join("carry/s.json")), 0o600);
    }
}

#[cfg(test)]
mod normalized_paths {
    use super::*;
    #[test]
    fn test_parent_segments_are_normalized_before_tightening() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, Permissions::from_mode(0o755)).unwrap();
        write_atomic(&root.join("../elsewhere/x"), "x", &root).unwrap();
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_to_string(d.path().join("elsewhere/x")).unwrap(),
            "x"
        );
    }
}

#[cfg(test)]
mod credential_tests {
    #[test]
    fn json_credential_keys_redact_short_and_nested_secrets() {
        let mut value = serde_json::json!({"token":"abc.def","password":"hunter2",
            "nested":[{"API-KEY":"short","Authorization":"plain","label":"ordinary"}]});
        super::redact_value(&mut value);
        assert_eq!(value["token"], "[redacted]");
        assert_eq!(value["password"], "[redacted]");
        assert_eq!(value["nested"][0]["API-KEY"], "[redacted]");
        assert_eq!(value["nested"][0]["Authorization"], "[redacted]");
        assert_eq!(value["nested"][0]["label"], "ordinary");
        assert_eq!(crate::redact::redact_private("hunter2"), "hunter2");
    }
}
