use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub fn default_root() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join(".claude-self-reflect/csr-companion")
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
    let path = std::path::absolute(path)?;
    let root = std::path::absolute(root)?;
    let inside = path.starts_with(&root);
    let first = if inside {
        root.as_path()
    } else {
        path.as_path()
    };
    if let Some(parent) = first.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::create_dir(first) {
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
            match fs::create_dir(&folder) {
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
    let path = std::path::absolute(path)?;
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
    let path = std::path::absolute(path)?;
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

pub fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = crate::redact::redact_private(text).into_owned(),
        serde_json::Value::Array(values) => values.iter_mut().for_each(redact_value),
        serde_json::Value::Object(values) => values.values_mut().for_each(redact_value),
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
        let mut v = serde_json::json!({"password":"sk-0123456789abcdefghij","n":3,"xs":["token=abc123",null]});
        redact_value(&mut v);
        assert_eq!(
            v,
            serde_json::json!({"password":"[redacted]","n":3,"xs":["token=[redacted]",null]})
        );
    }
}
