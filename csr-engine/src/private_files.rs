//! Owner-only permissions for the Self-Reflect data folder.
//!
//! The folder holds the conversation database and the hook timing log, so it
//! is 0700 and those files are 0600. New files are created that way, and
//! `repair_data_dir` fixes folders that earlier versions left at 0755/0644.

use std::fs;
use std::io;
use std::path::Path;

/// Directory name of the real data folder. Only a folder with this name is
/// ever chmod-ed as a directory, so a custom `--db-path` in a shared folder
/// never changes the permissions of that folder.
pub const DATA_DIR_NAME: &str = ".claude-self-reflect";
/// Files covered by name prefix: the database, `-wal`, `-shm` and backups.
const DB_PREFIX: &str = "csr-engine.db";
/// The timing log and its rotated copies.
const LOG_PREFIX: &str = "hook-timing.log";

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

/// Set `path` to `mode` when it differs. Symlinks are left alone: chmod would
/// follow them to a file outside the data folder. Returns true on a change.
#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || meta.permissions().mode() & 0o777 == mode {
        return Ok(false);
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(true)
}

#[cfg(not(unix))]
pub fn set_mode(_path: &Path, _mode: u32) -> io::Result<bool> {
    Ok(false)
}

/// Make the data folder 0700 (when it is the real one) and every database or
/// timing-log file in it 0600. Best effort: a failure on one entry never stops
/// the rest or the caller. Returns how many entries were changed.
pub fn repair_data_dir(dir: &Path) -> usize {
    let mut changed = 0;
    if dir.file_name().is_some_and(|n| n == DATA_DIR_NAME)
        && set_mode(dir, DIR_MODE).unwrap_or(false)
    {
        changed += 1;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return changed;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let private = name.starts_with(DB_PREFIX) || name.starts_with(LOG_PREFIX);
        if private
            && entry.file_type().is_ok_and(|t| t.is_file())
            && set_mode(&entry.path(), FILE_MODE).unwrap_or(false)
        {
            changed += 1;
        }
    }
    changed
}

/// Create the data folder (new folders are 0700), then repair what is in it.
pub fn create_data_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, DIR_MODE);
    builder.create(dir)?;
    repair_data_dir(dir);
    Ok(())
}

/// Open the database with owner-only files. A missing database file is created
/// 0600 first, so SQLite gives the `-wal` and `-shm` files the same mode; an
/// existing one is tightened, along with its `-wal` and `-shm`.
pub fn open_db(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    #[cfg(unix)]
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, FILE_MODE);
        let _ = options.open(path);
    }
    let conn = rusqlite::Connection::open(path)?;
    if let Some(dir) = path.parent() {
        repair_data_dir(dir);
    }
    Ok(conn)
}

/// Open `path` for appending, created 0600 when new.
pub fn append_private(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, FILE_MODE);
    options.open(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn chmod(path: &Path, m: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(m)).unwrap();
    }

    #[test]
    fn repair_turns_0644_files_and_0755_dir_private() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(DATA_DIR_NAME);
        fs::create_dir(&dir).unwrap();
        chmod(&dir, 0o755);
        let private = [
            "csr-engine.db",
            "csr-engine.db-wal",
            "csr-engine.db-shm",
            "hook-timing.log",
            "hook-timing.log.1",
        ];
        for name in private {
            fs::write(dir.join(name), b"x").unwrap();
            chmod(&dir.join(name), 0o644);
        }
        fs::write(dir.join("other.json"), b"x").unwrap();
        chmod(&dir.join("other.json"), 0o644);

        assert_eq!(repair_data_dir(&dir), 6);
        assert_eq!(mode(&dir), 0o700);
        for name in private {
            assert_eq!(mode(&dir.join(name)), 0o600, "{name}");
        }
        assert_eq!(mode(&dir.join("other.json")), 0o644, "unrelated file");
        assert_eq!(repair_data_dir(&dir), 0, "second pass changes nothing");
    }

    #[test]
    fn repair_leaves_a_custom_folder_and_symlinks_alone() {
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("shared");
        fs::create_dir(&shared).unwrap();
        chmod(&shared, 0o755);
        let outside = root.path().join("outside.txt");
        fs::write(&outside, b"x").unwrap();
        chmod(&outside, 0o644);
        std::os::unix::fs::symlink(&outside, shared.join("hook-timing.log")).unwrap();
        fs::write(shared.join("csr-engine.db"), b"x").unwrap();
        chmod(&shared.join("csr-engine.db"), 0o644);

        repair_data_dir(&shared);
        assert_eq!(mode(&shared), 0o755, "custom folder keeps its mode");
        assert_eq!(mode(&shared.join("csr-engine.db")), 0o600);
        assert_eq!(mode(&outside), 0o644, "symlink target untouched");
    }

    #[test]
    fn fresh_home_creates_private_folder_database_and_log() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(DATA_DIR_NAME);
        create_data_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);

        let db = dir.join("csr-engine.db");
        let storage = crate::storage::Storage::open(&db).unwrap();
        drop(storage);
        for name in ["csr-engine.db", "csr-engine.db-wal", "csr-engine.db-shm"] {
            let path = dir.join(name);
            if path.exists() {
                assert_eq!(mode(&path), 0o600, "{name}");
            }
        }
        assert_eq!(mode(&db), 0o600);

        let log = dir.join("hook-timing.log");
        append_private(&log).unwrap().write_all(b"line\n").unwrap();
        assert_eq!(mode(&log), 0o600);
    }

    #[test]
    fn open_db_tightens_an_existing_0644_database_and_its_wal() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(DATA_DIR_NAME);
        fs::create_dir(&dir).unwrap();
        let db = dir.join("csr-engine.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(a);")
                .unwrap();
            std::mem::forget(conn); // keep -wal/-shm on disk
        }
        chmod(&db, 0o644);
        chmod(&dir, 0o755);
        let conn = open_db(&db).unwrap();
        assert_eq!(mode(&db), 0o600);
        assert_eq!(mode(&dir), 0o700);
        for suffix in ["-wal", "-shm"] {
            let p = dir.join(format!("csr-engine.db{suffix}"));
            if p.exists() {
                assert_eq!(mode(&p), 0o600, "{suffix}");
            }
        }
        drop(conn);
    }
}
