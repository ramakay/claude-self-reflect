use std::path::{Path, PathBuf};

/// Keep fixture paths canonical, even if TMPDIR goes through a symlink.
pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)
}

/// The cache base that dirs::cache_dir() resolves for a child with an isolated
/// HOME and no XDG_CACHE_HOME override.
pub fn model_cache(home: &Path) -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        "Library/Caches"
    } else {
        ".cache"
    };
    home.join(base).join("csr-engine")
}

// The uninstall target only uses the path helpers.
#[allow(dead_code)]
pub fn copy_embedding_cache(home: &Path) {
    let source = csr_engine::embeddings::cache::cache_dir();
    if !source.exists() {
        return;
    }
    let target = model_cache(home).join("fastembed");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let flag = if cfg!(target_os = "macos") {
        "-Rc"
    } else {
        "-R"
    };
    let status = std::process::Command::new("cp")
        .arg(flag)
        .arg(&source)
        .arg(&target)
        .status()
        .unwrap();
    assert!(status.success(), "could not copy the local model cache");
}
