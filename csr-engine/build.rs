use std::path::PathBuf;
use std::process::Command;

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn absolute_git_path(path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

fn emit_git_reruns() {
    if let Some(head) = git_output(&["rev-parse", "--git-path", "HEAD"]) {
        println!(
            "cargo:rerun-if-changed={}",
            absolute_git_path(&head).display()
        );
    }
    if let Some(reference) = git_output(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git_output(&["rev-parse", "--git-path", &reference]) {
            println!(
                "cargo:rerun-if-changed={}",
                absolute_git_path(&path).display()
            );
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    let sha = git_output(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git_output(&["status", "--porcelain", "--untracked-files=no"])
        .map(|status| !status.is_empty())
        .unwrap_or(false);
    println!("cargo:rustc-env=CSR_BUILD_GIT_SHA={sha}");
    println!("cargo:rustc-env=CSR_BUILD_GIT_DIRTY={dirty}");

    emit_git_reruns();
}
