use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::Path,
};

pub const CHUNK: usize = 1024 * 1024;

#[derive(Default)]
pub struct Tail {
    pub offset: u64,
    pending: Vec<u8>,
    identity: Option<(u64, u64)>,
    matching: Matching,
}
impl Tail {
    pub fn at_end(path: &Path) -> io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        Ok(Self {
            offset: meta.len(),
            pending: vec![],
            identity: Some((meta.dev(), meta.ino())),
            matching: Matching::default(),
        })
    }
    /// Consume new bytes, retaining only the unfinished line between polls.
    pub fn poll(
        &mut self,
        path: &Path,
        budget: usize,
        mut stop: impl FnMut() -> bool,
        mut visit: impl FnMut(Option<&[u8]>),
    ) -> io::Result<()> {
        let mut file = File::open(path)?;
        let meta = file.metadata()?;
        let identity = (meta.dev(), meta.ino());
        if self.identity.is_some_and(|old| old != identity) || meta.len() < self.offset {
            self.offset = 0;
            self.pending.clear();
            visit(None);
        }
        self.identity = Some(identity);
        if meta.len() == self.offset {
            return Ok(());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut chunk = vec![0; CHUNK.min(budget)];
        let mut remaining = budget;
        while remaining > 0 && !stop() {
            let size = chunk.len().min(remaining);
            let n = file.read(&mut chunk[..size])?;
            if n == 0 {
                break;
            }
            self.offset += n as u64;
            remaining -= n;
            let mut consumed = 0;
            for part in chunk[..n].split_inclusive(|b| *b == b'\n') {
                if stop() {
                    self.offset -= (n - consumed) as u64;
                    return Ok(());
                }
                consumed += part.len();
                if part.ends_with(b"\n") {
                    if self.pending.is_empty() {
                        visit(Some(part));
                    } else {
                        self.pending.extend_from_slice(part);
                        visit(Some(&self.pending));
                        self.pending.clear();
                    }
                } else {
                    self.pending.extend_from_slice(part);
                }
            }
        }
        Ok(())
    }
}

/// A one-shot log read uses the same chunked reader, including a final line without LF.
pub fn scan(path: &Path, mut visit: impl FnMut(serde_json::Value)) {
    let mut tail = Tail::default();
    let mut line = |raw: &[u8]| {
        if let Ok(row) = sonic_rs::from_slice(raw) {
            visit(row);
        }
    };
    if tail
        .poll(
            path,
            usize::MAX,
            || false,
            |raw| {
                if let Some(raw) = raw {
                    line(raw);
                }
            },
        )
        .is_ok()
        && !tail.pending.is_empty()
    {
        line(&tail.pending);
    }
}

#[derive(Default)]
struct Matching {
    start: u64,
    seen: bool,
    prefix: Vec<u8>,
}
struct RangeReader<'a> {
    file: &'a File,
    at: u64,
    left: u64,
}
impl Read for RangeReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        use std::os::unix::fs::FileExt;
        let size = bytes.len().min(self.left as usize);
        let n = self.file.read_at(&mut bytes[..size], self.at)?;
        self.at += n as u64;
        self.left -= n as u64;
        Ok(n)
    }
}
impl Tail {
    /// Match across chunk boundaries without retaining unrelated, possibly huge lines.
    /// A matching line spanning chunks is replayed through a bounded streaming decoder.
    pub fn poll_matching<T: serde::de::DeserializeOwned>(
        &mut self,
        path: &Path,
        needle: &[u8],
        finish: bool,
        mut stop: impl FnMut() -> bool,
        mut visit: impl FnMut(Option<T>),
    ) -> io::Result<()> {
        let mut file = File::open(path)?;
        let meta = file.metadata()?;
        let identity = (meta.dev(), meta.ino());
        if self.identity.is_some_and(|old| old != identity) || meta.len() < self.offset {
            self.offset = 0;
            self.matching = Matching::default();
            visit(None);
        }
        self.identity = Some(identity);
        file.seek(SeekFrom::Start(self.offset))?;
        let finder = memchr::memmem::Finder::new(needle);
        let mut chunk = vec![0; CHUNK];
        let mut budget = if finish { usize::MAX } else { CHUNK };
        while budget > 0 && !stop() {
            let size = chunk.len().min(budget);
            let n = file.read(&mut chunk[..size])?;
            if n == 0 {
                break;
            }
            let chunk_at = self.offset;
            let mut low = 0;
            let ends = memchr::memchr_iter(b'\n', &chunk[..n])
                .map(|i| i + 1)
                .chain((chunk[n - 1] != b'\n').then_some(n));
            for high in ends {
                if stop() {
                    return Ok(());
                }
                let part = &chunk[low..high];
                let state = &mut self.matching;
                if !state.seen {
                    state.seen = finder.find(part).is_some();
                    if !state.seen && !state.prefix.is_empty() {
                        let take = needle.len().saturating_sub(1).min(part.len());
                        let mut edge = Vec::with_capacity(state.prefix.len() + take);
                        edge.extend_from_slice(&state.prefix);
                        edge.extend_from_slice(&part[..take]);
                        state.seen = finder.find(&edge).is_some();
                    }
                }
                self.offset = chunk_at + high as u64;
                if part.ends_with(b"\n") {
                    if state.seen {
                        let parsed = if state.start >= chunk_at {
                            sonic_rs::from_slice::<T>(
                                &chunk[(state.start - chunk_at) as usize..high],
                            )
                            .ok()
                        } else {
                            let reader = RangeReader {
                                file: &file,
                                at: state.start,
                                left: self.offset - state.start,
                            };
                            serde_json::from_reader::<_, T>(std::io::BufReader::new(reader)).ok()
                        };
                        if let Some(row) = parsed {
                            visit(Some(row));
                        }
                    }
                    state.start = self.offset;
                    state.seen = false;
                    state.prefix.clear();
                } else {
                    let keep = needle.len().saturating_sub(1);
                    if part.len() >= keep {
                        state.prefix.clear();
                    }
                    state
                        .prefix
                        .extend_from_slice(&part[part.len().saturating_sub(keep)..]);
                    if state.prefix.len() > keep {
                        state.prefix.drain(..state.prefix.len() - keep);
                    }
                }
                low = high;
            }
            budget -= n;
        }
        if finish
            && self.offset == meta.len()
            && self.matching.seen
            && self.matching.start < self.offset
        {
            let reader = RangeReader {
                file: &file,
                at: self.matching.start,
                left: self.offset - self.matching.start,
            };
            if let Ok(row) = serde_json::from_reader::<_, T>(std::io::BufReader::new(reader)) {
                visit(Some(row));
            }
            self.matching.start = self.offset;
            self.matching.seen = false;
            self.matching.prefix.clear();
        }
        Ok(())
    }
}

pub fn head(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_lines_advance_the_offset_and_are_not_read_again() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("log");
        std::fs::write(&p, b"one\npartial").unwrap();
        let mut tail = Tail::default();
        let mut lines = vec![];
        tail.poll(&p, CHUNK, || false, |s| lines.push(s.unwrap().to_vec()))
            .unwrap();
        assert_eq!(tail.offset, 11);
        tail.poll(&p, CHUNK, || false, |_| panic!("read old bytes"))
            .unwrap();
        assert_eq!(tail.pending, b"partial");
        assert_eq!(lines, [b"one\n"]);
    }
    #[test]
    fn filtered_lines_resume_without_retaining_unrelated_megabytes() {
        use std::io::Write;
        #[derive(serde::Deserialize)]
        struct Row {
            subtype: String,
            value: u64,
        }
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("transcript");
        let mut f = File::create(&p).unwrap();
        f.write_all(b"{\"blob\":\"").unwrap();
        let chunk = vec![b'a'; CHUNK];
        for _ in 0..16 {
            f.write_all(&chunk).unwrap();
        }
        f.write_all(b"\"}\n{\"subtype\":\"compact_boundary\",\"blob\":\"")
            .unwrap();
        for _ in 0..2 {
            f.write_all(&chunk).unwrap();
        }
        f.write_all(b"\",\"value\":42}").unwrap();
        drop(f);
        let mut tail = Tail::default();
        let mut found = vec![];
        tail.poll_matching::<Row>(
            &p,
            b"\"compact_boundary\"",
            false,
            || false,
            |_| panic!("no boundary in the first chunk"),
        )
        .unwrap();
        assert_eq!(tail.offset, CHUNK as u64);
        assert!(tail.matching.prefix.len() <= 17);
        tail.poll_matching::<Row>(
            &p,
            b"\"compact_boundary\"",
            true,
            || false,
            |r| {
                let r = r.unwrap();
                assert_eq!(r.subtype, "compact_boundary");
                found.push(r.value);
            },
        )
        .unwrap();
        assert_eq!(found, [42]);
        assert!(tail.matching.prefix.is_empty());
        assert!(tail.pending.is_empty());
    }
}
