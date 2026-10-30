use base64::{engine::general_purpose::STANDARD, Engine};
use std::sync::Arc;

pub fn pixels_enabled() -> bool {
    pixels_for(
        &std::env::var("TERM_PROGRAM").unwrap_or_default(),
        std::env::var("CSR_COMPANION_PIXELS").ok().as_deref(),
    )
}
pub fn pixels_for(program: &str, override_value: Option<&str>) -> bool {
    override_value != Some("0") && matches!(program, "herdr" | "kitty" | "ghostty" | "WezTerm")
}
thread_local! {
    static CELL_OVERRIDE: std::cell::Cell<Option<(u32, u32)>> = const { std::cell::Cell::new(None) };
}
/// Make this thread see terminal cells of `cell` pixels (None: ask the terminal). A headless run
/// has no tty to ask, so a test or a screenshot run uses it to draw at a real pane's pixel size.
pub fn set_cell_override(cell: Option<(u32, u32)>) {
    CELL_OVERRIDE.with(|c| c.set(cell));
}
pub fn cell_size() -> (u32, u32) {
    if let Some(cell) = CELL_OVERRIDE.with(std::cell::Cell::get) {
        return cell;
    }
    cell_from_window(crossterm::terminal::window_size().ok().as_ref())
}
fn cell_from_window(size: Option<&crossterm::terminal::WindowSize>) -> (u32, u32) {
    if let Some(s) = size {
        if s.width > 0 && s.height > 0 && s.columns > 0 && s.rows > 0 {
            return (
                u32::from((s.width / s.columns).max(6)) * 2,
                u32::from((s.height / s.rows).max(12)) * 2,
            );
        }
    }
    (16, 34)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub row: usize,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
    pub png: Arc<[u8]>,
}
pub fn kitty(png: &[u8], row: usize, col: usize, cols: usize, rows: usize) -> String {
    transmit(png, row, col, cols, rows, None)
}
/// `kitty` under image id `id`: sent again under the same id, a picture replaces the old one in
/// place, with no delete first. `z=-1` keeps it under the text: Warp paints a picture at z 0 or
/// above in a layer of its own, and that layer takes the click, so a tap on a card never arrives.
fn transmit(
    png: &[u8],
    row: usize,
    col: usize,
    cols: usize,
    rows: usize,
    id: Option<usize>,
) -> String {
    let mut out = format!("\x1b[{};{}H", row + 1, col + 1);
    let id = id.map(|id| format!("i={id},")).unwrap_or_default();
    let data = STANDARD.encode(png);
    let chunks = data.as_bytes().chunks(4096);
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let more = usize::from(index + 1 < count);
        let ctrl = if index == 0 {
            format!("a=T,f=100,q=2,C=1,{id}c={cols},r={rows},z=-1,m={more}")
        } else {
            format!("m={more}")
        };
        out.push_str(&format!(
            "\x1b_G{ctrl};{}\x1b\\",
            std::str::from_utf8(chunk).unwrap()
        ));
    }
    out
}
pub const SYNC_ON: &str = "\x1b[?2026h";
pub const SYNC_OFF: &str = "\x1b[?2026l";
pub const DELETE_ALL: &str = "\x1b_Ga=d,d=A\x1b\\";
/// `pictures` cut into one band per cell row. herdr sends a changed picture to the terminal as
/// raw pixels after deleting the old one, and the pane stays blank until they arrive: a whole
/// pane is about 2.7 MB. A band is one row of that, and an unchanged band is not sent at all.
pub fn bands(pictures: &[Placement]) -> Vec<Placement> {
    let mut out = vec![];
    for p in pictures {
        let Ok(img) = image::load_from_memory_with_format(&p.png, image::ImageFormat::Png) else {
            out.push(p.clone());
            continue;
        };
        let img = img.to_rgba8();
        let (width, height) = img.dimensions();
        if p.rows < 2 || height < p.rows as u32 {
            out.push(p.clone());
            continue;
        }
        let edge = |i: usize| (height as u64 * i as u64 / p.rows as u64) as u32;
        for i in 0..p.rows {
            let (top, bottom) = (edge(i), edge(i + 1));
            let band = image::imageops::crop_imm(&img, 0, top, width, bottom - top).to_image();
            let mut png = std::io::Cursor::new(Vec::new());
            if band.write_to(&mut png, image::ImageFormat::Png).is_err() {
                return pictures.to_vec();
            }
            out.push(Placement {
                row: p.row + i,
                col: p.col,
                cols: p.cols,
                rows: 1,
                png: Arc::from(png.into_inner()),
            });
        }
    }
    out
}
#[derive(Clone)]
pub struct Drawn {
    pub cols: usize,
    pub rows: usize,
    pub written: Vec<String>,
    pub placed: Vec<Placement>,
    /// The image id each placed picture is on screen under (`picture_ids`), one per `placed`.
    pub ids: Vec<usize>,
    /// The next image id this pane has never used. Ids only go up: a picture sent again, or sent
    /// after a full repaint, takes a fresh one and no id is ever sent a second time. 0: none yet,
    /// the process-wide clock decides.
    pub next_id: usize,
}
impl Drawn {
    /// What is on screen after `repaint_pictures` drew this frame over `drawn`.
    pub fn after(
        drawn: Option<&Drawn>,
        cols: usize,
        rows: usize,
        written: Vec<String>,
        placed: Vec<Placement>,
    ) -> Drawn {
        let Plan { ids, next_id } = plan(drawn, cols, rows, &written, &placed);
        publish(next_id);
        Drawn {
            cols,
            rows,
            written,
            placed,
            ids,
            next_id,
        }
    }
}
/// The highest image id sent. Ids stay under it so every one is a positive 31-bit number, inside
/// what any terminal takes; herdr hashes the pane and this id into its own host id.
const MAX_ID: usize = 0x7fff_ffff;
/// The clock starts below this at a random point, so a pane that restarts does not begin on the
/// ids its dead predecessor left in the terminal.
const START_SPREAD: usize = 1 << 24;
static CLOCK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The first id this process has never used, when no frame has been drawn yet.
fn clock() -> usize {
    use std::sync::atomic::Ordering::Relaxed;
    let now = CLOCK.load(Relaxed);
    if now != 0 {
        return now;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let seed = (nanos as usize ^ (std::process::id() as usize).rotate_left(17)) % START_SPREAD;
    match CLOCK.compare_exchange(0, seed + 1, Relaxed, Relaxed) {
        Ok(_) => seed + 1,
        Err(other) => other,
    }
}
/// Record that ids below `next` have been used.
fn publish(next: usize) {
    CLOCK.fetch_max(next, std::sync::atomic::Ordering::Relaxed);
}
/// The ids of a frame and the first id never used after it.
struct Plan {
    ids: Vec<usize>,
    next_id: usize,
}
/// Whether slot `i` is sent again this frame: a new, moved or changed picture, or one whose text
/// row was rewritten over it.
fn resent(same: Option<&Drawn>, written: &[String], placed: &[Placement], i: usize) -> bool {
    let shown = same.map(|d| d.placed.as_slice()).unwrap_or_default();
    let changed = |row: usize| same.and_then(|d| d.written.get(row)) != written.get(row);
    let under = |p: &Placement| (p.row..p.row + p.rows).any(changed);
    let Some(p) = placed.get(i) else {
        return false;
    };
    let old = shown.get(i);
    old != Some(p) || under(p) || old.is_some_and(under)
}
fn same_size(drawn: Option<&Drawn>, cols: usize, rows: usize) -> Option<&Drawn> {
    drawn.filter(|d| d.cols == cols && d.rows == rows)
}
/// The image id of each picture after this frame. A picture sent again takes a fresh id, never
/// one used before: the new one is placed before the old one is deleted, because herdr shows the
/// gap between a delete and the pixels that follow as a blank band, and a page scroll changes
/// every band at once. A picture left alone keeps its id.
pub fn picture_ids(
    drawn: Option<&Drawn>,
    cols: usize,
    rows: usize,
    written: &[String],
    placed: &[Placement],
) -> Vec<usize> {
    plan(drawn, cols, rows, written, placed).ids
}
fn plan(
    drawn: Option<&Drawn>,
    cols: usize,
    rows: usize,
    written: &[String],
    placed: &[Placement],
) -> Plan {
    let same = same_size(drawn, cols, rows);
    let mut next = match drawn {
        Some(d) if d.next_id > 0 => d.next_id,
        _ => clock(),
    };
    // The id a slot is on screen under, when it is on screen at all.
    let old_id = |i: usize| {
        same.filter(|d| i < d.placed.len())
            .and_then(|d| d.ids.get(i).copied())
    };
    // Past the top of the range the clock starts over; it skips any id still on screen.
    let on_screen: Vec<usize> = drawn.map(|d| d.ids.clone()).unwrap_or_default();
    let mut fresh = || loop {
        if next > MAX_ID {
            next = 1;
        }
        let id = next;
        next += 1;
        if !on_screen.contains(&id) {
            return id;
        }
    };
    let ids = (0..placed.len())
        .map(|i| match old_id(i) {
            Some(id) if !resent(same, written, placed, i) => id,
            _ => fresh(),
        })
        .collect();
    Plan { ids, next_id: next }
}
pub fn repaint(drawn: Option<&Drawn>, cols: usize, rows: usize, written: &[String]) -> String {
    repaint_pictures(drawn, cols, rows, written, &[])
}
pub fn repaint_pictures(
    drawn: Option<&Drawn>,
    cols: usize,
    rows: usize,
    written: &[String],
    placed: &[Placement],
) -> String {
    let same = same_size(drawn, cols, rows);
    let mut out = SYNC_ON.to_owned();
    let start = out.len();
    if same.is_none() {
        out.push_str(DELETE_ALL);
        out.push_str("\x1b[H\x1b[2J");
    }
    let shown = same.map(|d| d.placed.as_slice()).unwrap_or_default();
    let shown_id = |i: usize| same.and_then(|d| d.ids.get(i).copied());
    let ids = picture_ids(drawn, cols, rows, written, placed);
    for (i, line) in written.iter().enumerate() {
        if same.and_then(|d| d.written.get(i)) != Some(line) {
            out.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[2K{line}", i + 1));
        }
    }
    // New pictures first, each under an id never used, placed over the old one. A slot whose id
    // is the one it was shown under is left alone.
    for (i, p) in placed.iter().enumerate() {
        if shown_id(i) != Some(ids[i]) {
            out.push_str(&transmit(
                &p.png,
                p.row,
                p.col,
                p.cols,
                p.rows,
                Some(ids[i]),
            ));
        }
    }
    // Then the old ones go: a slot now under another id, or no longer drawn.
    for i in 0..shown.len() {
        if let Some(old) = shown_id(i).filter(|old| ids.get(i) != Some(old)) {
            out.push_str(&format!("\x1b_Ga=d,d=I,i={old},q=2;\x1b\\"));
        }
    }
    if out.len() == start {
        // Nothing changed. Any output at all makes herdr place every picture again.
        return String::new();
    }
    out.push_str(&format!("\x1b[{rows};{cols}H{SYNC_OFF}"));
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_repaint_writes_only_the_rows_that_changed() {
        let written = vec!["a".into(), "b".into()];
        let a = repaint(None, 46, 2, &written);
        assert!(a.contains("\x1b[2J"));
        let d = Drawn {
            cols: 46,
            rows: 2,
            written,
            placed: vec![],
            ids: vec![],
            next_id: 1,
        };
        let b = repaint(Some(&d), 46, 2, &["a".into(), "c".into()]);
        assert_eq!(
            b,
            format!("{SYNC_ON}\x1b[2;1H\x1b[0m\x1b[2Kc\x1b[2;46H{SYNC_OFF}")
        );
        assert!(repaint(Some(&d), 47, 2, &d.written).contains("\x1b[2J"));
    }
}

#[cfg(test)]
mod picture_tests {
    use super::*;
    #[test]
    fn test_a_frame_rewrites_only_the_rows_that_changed_and_never_clears_the_screen() {
        let written = ["a", "b", "c", "legend"].map(str::to_owned).to_vec();
        assert!(repaint(None, 10, 4, &written).contains("\x1b[2J"));
        let drawn = Drawn {
            cols: 10,
            rows: 4,
            written,
            placed: vec![],
            ids: vec![],
            next_id: 1,
        };
        assert_eq!(repaint(Some(&drawn), 10, 4, &drawn.written), "");
        let mut next = drawn.written.clone();
        next[1] = "B".into();
        let moved = repaint(Some(&drawn), 10, 4, &next);
        assert!(!moved.contains("\x1b[2J"));
        assert!(moved.contains("\x1b[2;1H\x1b[0m\x1b[2KB"));
        assert!(!moved.contains("\x1b[1;1H"));
        assert!(moved.starts_with(SYNC_ON) && moved.ends_with(SYNC_OFF));
        assert!(repaint(Some(&drawn), 12, 4, &next).contains("\x1b[2J"));
    }
    #[test]
    fn test_pictures_are_sent_again_only_when_they_or_the_rows_under_them_change() {
        let picture = Placement {
            row: 1,
            col: 2,
            cols: 4,
            rows: 2,
            png: Arc::from(b"png".as_slice()),
        };
        let written = ["a", "b", "c", "legend"].map(str::to_owned).to_vec();
        let drawn = Drawn {
            cols: 10,
            rows: 4,
            written,
            placed: vec![picture.clone()],
            ids: vec![1],
            next_id: 2,
        };
        let mut next = drawn.written.clone();
        next[0] = "A".into();
        assert!(
            !repaint_pictures(Some(&drawn), 10, 4, &next, std::slice::from_ref(&picture))
                .contains("\x1b_G")
        );
        next = drawn.written.clone();
        next[2] = "C".into();
        let under = repaint_pictures(Some(&drawn), 10, 4, &next, std::slice::from_ref(&picture));
        // Sent again under a fresh id, and the old id deleted only after it.
        let (placed_at, deleted_at) = (
            under.find("a=T,f=100,q=2,C=1,i=2,").unwrap(),
            under.find("a=d,d=I,i=1,q=2;").unwrap(),
        );
        assert!(placed_at < deleted_at);
        let gone = repaint_pictures(Some(&drawn), 10, 4, &drawn.written, &[]);
        assert!(gone.contains("a=d,d=I,i=1,q=2;") && !gone.contains("a=T"));
        let mut changed = picture;
        changed.png = Arc::from(b"new".as_slice());
        assert!(repaint_pictures(Some(&drawn), 10, 4, &drawn.written, &[changed]).contains("a=T"));
    }
    fn solid(width: u32, height: u32, bands: impl Fn(usize) -> u8, count: usize) -> Arc<[u8]> {
        let mut img = image::RgbaImage::new(width, height);
        for (y, row) in img.rows_mut().enumerate() {
            let level = bands(y * count / height as usize);
            row.for_each(|p| *p = image::Rgba([level, level, level, u8::MAX]));
        }
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        Arc::from(out.into_inner())
    }
    #[test]
    fn a_picture_is_cut_into_one_band_per_cell_row() {
        let whole = Placement {
            row: 2,
            col: 1,
            cols: 4,
            rows: 3,
            png: solid(8, 6, |i| 1 + 3 * i as u8, 3),
        };
        let cut = bands(std::slice::from_ref(&whole));
        assert_eq!(cut.len(), 3);
        for (i, band) in cut.iter().enumerate() {
            assert_eq!((band.row, band.col, band.cols, band.rows), (2 + i, 1, 4, 1));
            let img = image::load_from_memory(&band.png).unwrap().to_rgba8();
            assert_eq!(img.dimensions(), (8, 2));
            assert_eq!(img.get_pixel(0, 0).0[0], 1 + 3 * i as u8);
        }
    }
    #[test]
    fn every_picture_sits_under_the_text_so_a_tap_on_it_arrives() {
        let png = vec![0u8; 5000];
        let sent = kitty(&png, 2, 1, 4, 3);
        let first = sent.split("\x1b_G").nth(1).unwrap();
        assert!(
            first.starts_with("a=T,") && first.contains(",z=-1,"),
            "{first:.60}"
        );
        assert!(transmit(&png, 0, 0, 1, 1, Some(7)).contains(",i=7,c=1,r=1,z=-1,"));
    }
    #[test]
    fn only_the_band_that_changed_is_sent_again_and_its_old_id_goes_only_after_it() {
        let before = bands(&[Placement {
            row: 0,
            col: 0,
            cols: 4,
            rows: 3,
            png: solid(8, 6, |i| i as u8, 3),
        }]);
        let after = bands(&[Placement {
            row: 0,
            col: 0,
            cols: 4,
            rows: 3,
            png: solid(8, 6, |i| if i == 1 { 9 } else { i as u8 }, 3),
        }]);
        let written = vec![String::new(); 4];
        let drawn = Drawn {
            cols: 4,
            rows: 4,
            written: written.clone(),
            placed: before.clone(),
            ids: (1..=3).collect(),
            next_id: 4,
        };
        let out = repaint_pictures(Some(&drawn), 4, 4, &written, &after);
        // herdr draws the gap between a delete and the next upload as a blank band: the new
        // picture is placed under a fresh id first, and only then is the old id deleted.
        assert_eq!(out.matches("a=T").count(), 1);
        assert_eq!(out.matches("a=d").count(), 1);
        assert!(
            out.find("a=T,f=100,q=2,C=1,i=4,").unwrap() < out.find("a=d,d=I,i=2,q=2;").unwrap()
        );
        assert!(!out.contains(DELETE_ALL) && !out.contains("\x1b[2J"));
        // The next change takes the next fresh id, not the one the band had before.
        let ids = picture_ids(Some(&drawn), 4, 4, &written, &after);
        assert_eq!((ids[0], ids[1], ids[2]), (1, 4, 3));
        let again = Drawn {
            placed: after.clone(),
            ids,
            next_id: 5,
            ..drawn.clone()
        };
        let back = repaint_pictures(Some(&again), 4, 4, &written, &before);
        assert!(
            back.find("a=T,f=100,q=2,C=1,i=5,").unwrap() < back.find("a=d,d=I,i=4,q=2;").unwrap()
        );
        assert!(!back.contains("i=2,"));
        assert_eq!(repaint_pictures(Some(&drawn), 4, 4, &written, &before), "");
        let gone = repaint_pictures(Some(&drawn), 4, 4, &written, &after[..1]);
        assert!(gone.contains("a=d,d=I,i=2,q=2;") && gone.contains("a=d,d=I,i=3,q=2;"));
        assert!(!gone.contains(DELETE_ALL));
    }
    /// The image ids a frame's bytes place (`a=T`) and delete (`a=d,d=I`), in order of appearance.
    fn ids_in(out: &str) -> (Vec<usize>, Vec<usize>) {
        let id_of = |at: &str| -> usize {
            let (_, tail) = at.split_once(",i=").unwrap();
            tail.split(',').next().unwrap().parse().unwrap()
        };
        let placed = out.split("\x1b_Ga=T,").skip(1).map(id_of).collect();
        let deleted = out.split("\x1b_Ga=d,d=I").skip(1).map(id_of).collect();
        (placed, deleted)
    }
    #[test]
    fn an_image_id_is_never_sent_again_and_every_delete_names_an_id_that_is_up() {
        use std::collections::HashSet;
        // A small deterministic generator: no frame order is hand-picked.
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut roll = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let (mut cols, mut rows) = (35, 30);
        let mut level = vec![0u8; 29];
        let mut written: Vec<String> = (0..rows).map(|i| format!("row {i}")).collect();
        let mut drawn: Option<Drawn> = None;
        let (mut ever, mut live, mut deleted_ids): (
            HashSet<usize>,
            HashSet<usize>,
            HashSet<usize>,
        ) = Default::default();
        let (mut sent, mut deleted, mut resizes) = (0, 0, 0);
        for frame in 0..1000 {
            match roll(10) {
                // A few bands change.
                0..=5 => (0..1 + roll(4)).for_each(|_| {
                    let at = roll(level.len());
                    level[at] = level[at].wrapping_add(1 + roll(250) as u8);
                }),
                // A text row under a band is rewritten.
                6 | 7 => {
                    let at = roll(rows);
                    written[at] = format!("row {at} frame {frame}");
                }
                // The pane is resized, or gains and loses bands.
                8 if roll(4) == 0 => {
                    resizes += 1;
                    cols = 30 + roll(20);
                    rows = 20 + roll(15);
                    written = (0..rows).map(|i| format!("row {i}")).collect();
                }
                8 => level.truncate(1 + roll(level.len())),
                _ => level.resize(level.len() + roll(5), 7),
            }
            let placed: Vec<Placement> = level
                .iter()
                .enumerate()
                .map(|(row, l)| Placement {
                    row,
                    col: 0,
                    cols,
                    rows: 1,
                    png: Arc::from(vec![*l; 8]),
                })
                .collect();
            let out = repaint_pictures(drawn.as_ref(), cols, rows, &written, &placed);
            let next = Drawn::after(drawn.as_ref(), cols, rows, written.clone(), placed);
            if out.contains(DELETE_ALL) {
                deleted_ids.extend(live.drain());
            }
            let (up, down) = ids_in(&out);
            // Every picture goes up before any old one comes down.
            if let (Some(last_up), Some(first_down)) = (out.rfind("a=T,"), out.find("a=d,d=I")) {
                assert!(last_up < first_down, "frame {frame}: delete before place");
            }
            for id in up {
                assert!(
                    (1..=MAX_ID).contains(&id),
                    "frame {frame}: id {id} out of range"
                );
                assert!(ever.insert(id), "frame {frame}: id {id} was sent before");
                live.insert(id);
                sent += 1;
            }
            for id in down {
                assert!(
                    ever.contains(&id),
                    "frame {frame}: delete of id {id} never placed"
                );
                assert!(live.remove(&id), "frame {frame}: delete of id {id} not up");
                assert!(
                    deleted_ids.insert(id),
                    "frame {frame}: id {id} deleted twice"
                );
                deleted += 1;
            }
            let shown: HashSet<usize> = next.ids.iter().copied().collect();
            assert_eq!(live, shown, "frame {frame}: ids up differ from ids drawn");
            assert_eq!(next.ids.len(), next.placed.len());
            drawn = Some(next);
        }
        eprintln!("1000 frames: {sent} ids sent, {deleted} deleted, {resizes} resizes");
        assert!(
            sent > 1000 && deleted > 500 && resizes > 5,
            "the run proves nothing"
        );
    }
    #[test]
    fn the_id_clock_starts_over_below_the_top_without_taking_an_id_on_screen() {
        let pic = |row: usize, tag: u8| Placement {
            row,
            col: 0,
            cols: 4,
            rows: 1,
            png: Arc::from(vec![tag; 4]),
        };
        let written = vec![String::new(); 4];
        let before: Vec<Placement> = (0..3).map(|r| pic(r, 0)).collect();
        let drawn = Drawn {
            cols: 4,
            rows: 4,
            written: written.clone(),
            placed: before,
            ids: vec![MAX_ID - 1, 1, 2],
            next_id: MAX_ID,
        };
        let after: Vec<Placement> = (0..3).map(|r| pic(r, 1)).collect();
        let ids = picture_ids(Some(&drawn), 4, 4, &written, &after);
        assert_eq!(
            ids,
            [MAX_ID, 3, 4],
            "past the top it starts over, skipping 1 and 2"
        );
        assert!(ids
            .iter()
            .all(|id| (1..=MAX_ID).contains(id) && !drawn.ids.contains(id)));
    }
    #[test]
    fn kitty_chunks_transmit_png_and_place_without_moving_the_cursor() {
        let png = vec![42; 7000];
        let output = kitty(&png, 2, 3, 40, 5);
        assert!(output.starts_with("\x1b[3;4H\x1b_Ga=T,f=100,q=2,C=1,c=40,r=5,z=-1,m=1;"));
        let mut encoded = String::new();
        let mut lengths = vec![];
        for packet in output.split("\x1b_G").skip(1) {
            let (ctrl, rest) = packet.split_once(';').unwrap();
            let data = rest.strip_suffix("\x1b\\").unwrap();
            assert!(ctrl.ends_with("m=0") || ctrl.ends_with("m=1"));
            lengths.push(data.len());
            encoded.push_str(data);
        }
        assert_eq!(lengths, vec![4096, 4096, 1144]);
        assert_eq!(STANDARD.decode(encoded).unwrap(), png);
    }
    #[test]
    fn cell_size_and_pixels_gate_match_the_python_clients() {
        use crossterm::terminal::WindowSize;
        assert_eq!(cell_from_window(None), (16, 34));
        let mut size = WindowSize {
            columns: 80,
            rows: 24,
            width: 640,
            height: 408,
        };
        assert_eq!(cell_from_window(Some(&size)), (16, 34));
        size.width = 0;
        assert_eq!(cell_from_window(Some(&size)), (16, 34));
        size.width = 80;
        size.height = 24;
        assert_eq!(cell_from_window(Some(&size)), (12, 24));
        for name in ["herdr", "kitty", "ghostty", "WezTerm"] {
            assert!(pixels_for(name, None));
            assert!(!pixels_for(name, Some("0")));
        }
        for name in ["", "wezterm", "Terminal.app", "vscode"] {
            assert!(!pixels_for(name, None));
        }
    }
}
