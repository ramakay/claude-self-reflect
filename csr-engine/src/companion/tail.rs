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
}
impl Tail {
    pub fn at_end(path: &Path) -> io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        Ok(Self {
            offset: meta.len(),
            pending: vec![],
            identity: Some((meta.dev(), meta.ino())),
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
}
