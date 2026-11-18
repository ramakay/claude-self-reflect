//! The instrument picture's big buffers, kept from one frame to the next.
//!
//! A frame paints a canvas (about 10 MB at a 35-column pane), resizes it into a second buffer and
//! lays that on a third, which becomes the picture. Allocated and freed every frame, blocks that
//! size are not returned to the system: the allocator keeps them in its large-block cache, and
//! the freed frames sat there as resident memory (69 MB of an idle pane's 207). Here the three
//! come from a short list of buffers the last frame gave back, so an idle pane holds the same
//! few blocks for good. A buffer is looked up by its exact length: a pane resize asks for new
//! sizes, the old ones are dropped as the list turns over, and the list never holds more than
//! [`KEEP`] buffers or [`BYTES`] bytes.
use std::sync::Mutex;

/// Most buffers kept.
const KEEP: usize = 4;
/// Most bytes kept in all.
const BYTES: usize = 48 << 20;
/// Smaller buffers are cheap to allocate and are not kept.
const SMALLEST: usize = 64 << 10;

/// The buffers given back, oldest first.
struct Pool(Mutex<Vec<Vec<u8>>>);

impl Pool {
    const fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }
    fn list(&self) -> std::sync::MutexGuard<'_, Vec<Vec<u8>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn take(&self, len: usize) -> Vec<u8> {
        let mut kept = self.list();
        match kept.iter().position(|b| b.len() == len) {
            Some(at) => kept.swap_remove(at),
            None => {
                drop(kept);
                vec![0; len]
            }
        }
    }
    fn keep(&self, buffer: Vec<u8>) {
        if buffer.len() < SMALLEST {
            return;
        }
        let mut kept = self.list();
        kept.push(buffer);
        // The oldest go first.
        while kept.len() > KEEP || kept.iter().map(Vec::len).sum::<usize>() > BYTES {
            kept.remove(0);
        }
    }
}

static FREE: Pool = Pool::new();

/// When a frame last took a buffer, and whether the caches were let go of since. A pane that
/// paints no picture for a while (most of an idle pane's life) need not hold a frame's worth of
/// buffers: they go, and the next picture allocates them again.
#[derive(Default)]
struct Quiet {
    last: Option<std::time::Instant>,
    drained: bool,
}
impl Quiet {
    fn took(&mut self, at: std::time::Instant) {
        self.last = Some(at);
        self.drained = false;
    }
    /// True once per quiet spell: when `quiet` has passed since the last take.
    fn due(&mut self, now: std::time::Instant, quiet: std::time::Duration) -> bool {
        let due = !self.drained
            && self
                .last
                .is_some_and(|at| now.saturating_duration_since(at) >= quiet);
        self.drained |= due;
        due
    }
}
static QUIET: Mutex<Quiet> = Mutex::new(Quiet {
    last: None,
    drained: false,
});
fn quiet() -> std::sync::MutexGuard<'static, Quiet> {
    QUIET.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether no frame has taken a buffer for `quiet`; true once per quiet spell.
pub fn quiet_due(now: std::time::Instant, quiet_for: std::time::Duration) -> bool {
    quiet().due(now, quiet_for)
}

/// Let every kept buffer go.
pub fn drain() {
    FREE.list().clear();
}

/// A buffer of `len` bytes holding whatever the last user left in it: for one that is written
/// all over before it is read.
pub fn take(len: usize) -> Vec<u8> {
    quiet().took(std::time::Instant::now());
    FREE.take(len)
}

/// A buffer of `len` zero bytes, as a new pixmap's are.
pub fn zeroed(len: usize) -> Vec<u8> {
    let mut buffer = take(len);
    buffer.fill(0);
    buffer
}

/// Gives a buffer back for the next frame.
pub fn keep(buffer: Vec<u8>) {
    FREE.keep(buffer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_given_back_buffer_comes_again_and_the_list_is_bounded() {
        let pool = Pool::new();
        let len = 1_000_003;
        let mut first = pool.take(len);
        assert_eq!(first.len(), len);
        first.fill(7);
        let at = first.as_ptr();
        pool.keep(first);
        let again = pool.take(len);
        assert_eq!(again.as_ptr(), at, "the same block comes back");
        assert!(again.iter().all(|b| *b == 7), "as it was left");
        // Nothing of another length is handed out.
        pool.keep(again);
        let other = pool.take(len + 4);
        assert_ne!(other.as_ptr(), at);
        assert_eq!(pool.list().len(), 1, "the first block is still kept");
        // Small buffers are not kept, and the list stops at its bounds.
        pool.keep(vec![1u8; 1000]);
        assert_eq!(pool.list().len(), 1, "only the first block");
        for i in 0..2 * KEEP {
            pool.keep(vec![0; SMALLEST + 4096 * (i + 1)]);
        }
        assert!(pool.list().len() <= KEEP);
        for i in 0..4 {
            pool.keep(vec![0; BYTES / 3 + 4096 * (i + 1)]);
        }
        assert!(pool.list().iter().map(Vec::len).sum::<usize>() <= BYTES);
    }

    #[test]
    fn the_caches_go_once_a_quiet_spell_and_stay_while_frames_are_drawn() {
        use std::time::{Duration, Instant};
        let wait = Duration::from_secs(5);
        let t0 = Instant::now();
        let mut q = Quiet::default();
        assert!(
            !q.due(t0 + wait * 9, wait),
            "nothing drawn yet: nothing kept"
        );
        q.took(t0);
        // Frames every second: never due.
        for s in 1..20 {
            assert!(!q.due(t0 + Duration::from_secs(s), wait));
            q.took(t0 + Duration::from_secs(s));
        }
        let last = t0 + Duration::from_secs(19);
        assert!(!q.due(last + wait - Duration::from_millis(1), wait));
        assert!(q.due(last + wait, wait), "five quiet seconds: due");
        assert!(!q.due(last + wait * 3, wait), "once per quiet spell");
        q.took(last + wait * 4);
        assert!(q.due(last + wait * 5, wait), "a new spell after a frame");
        // Draining the pool lets its blocks go.
        keep(vec![0; SMALLEST * 2]);
        assert!(!FREE.list().is_empty());
        drain();
        assert!(FREE.list().is_empty());
    }

    #[test]
    fn zeroed_buffers_are_zero_whatever_they_held() {
        let len = 200_001;
        let mut dirty = take(len);
        dirty.fill(9);
        keep(dirty);
        assert!(zeroed(len).iter().all(|b| *b == 0));
    }
}
