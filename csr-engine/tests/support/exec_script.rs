//! Writing a program a test is about to run.
//!
//! A parallel test can fork while this process holds the new file's write handle. The child keeps
//! a copy of the handle until its own exec, and an exec of the file in that time fails with
//! ETXTBSY ("Text file busy"). Seen on the Linux CI runners, in code under test that runs the
//! program, where a test cannot retry the spawn. So the write is done by a child process that is
//! awaited: this process never holds a write handle on the program, and no fork of it can copy one.
//!
//! Included with `#[path]` by every test crate that makes a program (the library's own tests too).
#![allow(dead_code)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Write `contents` to `path` and make it executable (mode 0755).
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>) {
    let mut writer = Command::new("/bin/sh")
        .args(["-c", r#"/bin/cat > "$0" && /bin/chmod 755 "$0""#])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("start the program writer");
    writer
        .stdin
        .take()
        .expect("the writer's stdin")
        .write_all(contents.as_ref())
        .expect("feed the program writer");
    assert!(writer.wait().expect("wait for the writer").success());
}

/// Copy the program `from` to `to`, the same way, and make it executable.
pub fn copy_executable(from: &Path, to: &Path) {
    let status = Command::new("/bin/cp")
        .arg(from)
        .arg(to)
        .status()
        .expect("start the program copier");
    assert!(status.success());
}
