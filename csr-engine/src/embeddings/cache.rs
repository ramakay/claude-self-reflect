use std::path::PathBuf;

/// Returns the cache directory for fastembed model files.
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("csr-engine")
        .join("fastembed")
}
