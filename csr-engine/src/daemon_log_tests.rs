//! Tests use spare descriptors. None touches the test process's own fd 0-2.

use super::*;

#[test]
fn rotation_is_decided_by_size_over_the_cap() {
    assert!(!should_rotate(0, LOG_CAP_BYTES));
    assert!(!should_rotate(LOG_CAP_BYTES, LOG_CAP_BYTES));
    assert!(should_rotate(LOG_CAP_BYTES + 1, LOG_CAP_BYTES));
    // The 19.6 MB log of the adopter report is over a 10 MiB cap.
    assert!(should_rotate(19_608_764, LOG_CAP_BYTES));
    assert_eq!(
        rotated_path(Path::new("/x/logs/daemon.err.log")),
        PathBuf::from("/x/logs/daemon.err.log.1")
    );
}

#[cfg(unix)]
mod on_a_temp_file {
    use super::*;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};

    /// Two spare descriptors on one file, standing in for fd 2 and fd 1.
    fn spare_pair(path: &Path) -> (File, File) {
        let first = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        // SAFETY: dup returns a new descriptor this test owns.
        let second = unsafe { File::from_raw_fd(libc::dup(first.as_raw_fd())) };
        (first, second)
    }

    #[test]
    fn start_time_rotation_renames_to_dot_one_and_reopens_a_fresh_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.err.log");
        std::fs::write(&path, vec![b'o'; 500]).unwrap();
        let (mut err, mut out) = spare_pair(&path);

        let rotated = unix::rotate_fd(err.as_raw_fd(), out.as_raw_fd(), 100).unwrap();
        assert!(rotated.is_some());
        assert_eq!(std::fs::read(rotated_path(&path)).unwrap(), vec![b'o'; 500]);
        assert_eq!(std::fs::read(&path).unwrap().len(), 0);

        // Both descriptors now write to the fresh file; the copy is untouched.
        err.write_all(b"from stderr\n").unwrap();
        out.write_all(b"from stdout\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "from stderr\nfrom stdout\n"
        );
        assert_eq!(std::fs::read(rotated_path(&path)).unwrap().len(), 500);
    }

    #[test]
    fn a_second_rotation_replaces_the_one_kept_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.err.log");
        std::fs::write(&path, vec![b'a'; 300]).unwrap();
        let (err, out) = spare_pair(&path);
        unix::rotate_fd(err.as_raw_fd(), out.as_raw_fd(), 100).unwrap();
        std::fs::write(&path, vec![b'b'; 300]).unwrap();
        let (err, out) = spare_pair(&path);
        unix::rotate_fd(err.as_raw_fd(), out.as_raw_fd(), 100).unwrap();
        assert_eq!(std::fs::read(rotated_path(&path)).unwrap(), vec![b'b'; 300]);
        assert!(!dir.path().join("daemon.err.log.2").exists());
    }

    #[test]
    fn a_log_under_the_cap_and_a_non_file_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.err.log");
        std::fs::write(&path, vec![b'u'; 50]).unwrap();
        let (err, out) = spare_pair(&path);
        assert_eq!(
            unix::rotate_fd(err.as_raw_fd(), out.as_raw_fd(), 100).unwrap(),
            None
        );
        assert!(!rotated_path(&path).exists());
        let null = File::open("/dev/null").unwrap();
        assert_eq!(
            unix::rotate_fd(null.as_raw_fd(), out.as_raw_fd(), 0).unwrap(),
            None
        );
    }
}
