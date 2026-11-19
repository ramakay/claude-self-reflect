mod bufpool;
pub mod cards;
pub mod instrument;
mod instrument_gfx;
mod instrument_icons;
pub mod kitty;
pub mod observation;
pub mod pictures;
pub mod raster;
pub(super) mod resample;
pub mod text;
pub mod theme;

/// How long a pane paints no picture before its frame caches go.
pub const FRAME_CACHE_QUIET: std::time::Duration = std::time::Duration::from_secs(5);

/// Let the frame caches go (the buffer pool, the kept backgrounds and shadow masks: 35 MB in an
/// awake pane) once no picture has been painted for [`FRAME_CACHE_QUIET`]; while frames are
/// painted they stay. Once per quiet spell; true when it let them go.
pub fn drain_frame_caches_if_quiet(now: std::time::Instant) -> bool {
    if !bufpool::quiet_due(now, FRAME_CACHE_QUIET) {
        return false;
    }
    bufpool::drain();
    instrument_gfx::drain_caches();
    true
}

/// The bytes of one font file, loaded once per process and kept for its life. Every face made from
/// a file borrows this one copy, whatever its weight or size and whichever thread draws with it.
///
/// A font on the macOS system volume is mapped, not read: its pages are the file's own, clean and
/// shared with every other pane, and only the glyphs drawn are ever paged in. Read whole, the
/// system fallbacks cost each awake pane 36 MB of its own memory (Arial Unicode alone is 22 MB).
/// Every other font is read whole, as a file someone may rewrite in place must not be mapped.
pub(crate) fn font_bytes(path: &str) -> Option<&'static [u8]> {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    static FILES: LazyLock<Mutex<HashMap<String, &'static [u8]>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut files = FILES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(bytes) = files.get(path) {
        return Some(bytes);
    }
    let mapped = if maps(path) { map_file(path) } else { None };
    let bytes: &'static [u8] = match mapped {
        Some(bytes) => bytes,
        None => Box::leak(std::fs::read(path).ok()?.into_boxed_slice()),
    };
    files.insert(path.to_string(), bytes);
    Some(bytes)
}

/// Whether a font file is mapped: only one on the sealed, read-only system volume of macOS
/// (`/System/Library/`). A file anyone can rewrite in place (`/Library/Fonts`, a user's
/// `Library/Fonts`, every Linux folder) is read into owned bytes: truncated while mapped, it
/// would kill the pane with SIGBUS on the next glyph read.
fn maps(path: &str) -> bool {
    maps_on(cfg!(target_os = "macos"), path)
}

/// [`maps`] for a given host, so both answers can be tested on any one.
fn maps_on(macos: bool, path: &str) -> bool {
    macos && path.starts_with("/System/Library/")
}

/// A read-only private mapping of the whole file, kept for the life of the process (the
/// descriptor can close: the mapping holds the file). None for an empty file or a failed map.
#[cfg(unix)]
fn map_file(path: &str) -> Option<&'static [u8]> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(path).ok()?;
    let len = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if len == 0 {
        return None;
    }
    // SAFETY: a fresh read-only mapping of `len` bytes of an open file; it is never unmapped, so
    // the slice stays valid for the life of the process.
    let at = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if at == libc::MAP_FAILED {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(at.cast::<u8>(), len) })
}
#[cfg(not(unix))]
fn map_file(_: &str) -> Option<&'static [u8]> {
    None
}

#[cfg(test)]
mod font_map_tests {
    use super::{map_file, maps, maps_on};
    #[test]
    fn only_the_sealed_macos_system_volume_is_mapped_and_every_other_font_is_read() {
        // macOS: the sealed volume maps.
        for path in [
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            "/System/Library/Fonts/Menlo.ttc",
            "/System/Library/Fonts/SFNS.ttf",
        ] {
            assert!(maps_on(true, path), "{path}");
        }
        // macOS: a font anyone can rewrite is read, whatever the folder.
        for path in [
            "/Library/Fonts/Arial Unicode.ttf",
            "/Users/someone/Library/Fonts/Inter.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/tmp/System/Library/Fonts/x.ttf",
            "System/Library/Fonts/x.ttf",
        ] {
            assert!(!maps_on(true, path), "{path}");
        }
        // Linux: nothing maps, not even a path that looks like the Mac's, nor the folder a
        // package manager replaces files in.
        for path in [
            "/System/Library/Fonts/Menlo.ttc",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/opentype/inter/Inter-Regular.otf",
        ] {
            assert!(!maps_on(false, path), "{path}");
        }
        // The host's own answer is the macOS one on a Mac and the Linux one elsewhere.
        assert_eq!(
            maps("/System/Library/Fonts/Menlo.ttc"),
            cfg!(target_os = "macos")
        );
        assert!(!maps("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"));
    }
    #[test]
    fn a_mapped_file_reads_as_its_bytes_and_an_empty_one_is_not_mapped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("face.ttf");
        let bytes: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let mapped = map_file(path.to_str().unwrap()).unwrap();
        assert_eq!(mapped, &bytes[..]);
        // The mapping outlives the name.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(mapped[69_999], bytes[69_999]);
        std::fs::write(&path, b"").unwrap();
        assert!(map_file(path.to_str().unwrap()).is_none());
        assert!(map_file("/nonexistent/csr-font.ttf").is_none());
    }
}

#[cfg(test)]
mod font_bytes_tests {
    use super::font_bytes;
    #[test]
    fn one_file_is_read_once_and_every_call_gets_the_same_bytes() {
        let path = std::env::temp_dir().join(format!("csr-font-bytes-{}.bin", std::process::id()));
        std::fs::write(&path, b"not really a font").unwrap();
        let p = path.to_str().unwrap();
        let a = font_bytes(p).unwrap();
        // the file changing or going away must not matter: the process already holds its copy
        std::fs::remove_file(&path).unwrap();
        let b = font_bytes(p).unwrap();
        assert_eq!(a, b"not really a font");
        assert!(std::ptr::eq(a.as_ptr(), b.as_ptr()));
        assert!(font_bytes("/nonexistent/csr-font-bytes.ttf").is_none());
    }
}
