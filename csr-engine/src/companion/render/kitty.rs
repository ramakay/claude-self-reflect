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
    pub png: Arc<Png>,
}
/// A picture's PNG. One made from pixels is encoded only when its bytes are asked for: a band
/// already up in the terminal is placed again by its id and never encoded. Two pictures are equal
/// when their content hashes are, so the same pixels are the same picture.
pub struct Png {
    pixels: Option<tiny_skia::Pixmap>,
    bytes: std::sync::OnceLock<Arc<[u8]>>,
    hash: u64,
}
fn hash_of(parts: &[&[u8]]) -> u64 {
    let mut h = blake3::Hasher::new();
    for part in parts {
        h.update(part);
    }
    let out = h.finalize();
    u64::from_le_bytes(out.as_bytes()[..8].try_into().unwrap_or_default())
}
impl Png {
    pub fn from_bytes(bytes: impl Into<Arc<[u8]>>) -> Arc<Png> {
        let bytes: Arc<[u8]> = bytes.into();
        Arc::new(Png {
            pixels: None,
            hash: hash_of(&[b"png", &bytes]),
            bytes: std::sync::OnceLock::from(bytes),
        })
    }
    pub fn from_pixmap(pixmap: tiny_skia::Pixmap) -> Arc<Png> {
        let dims = [pixmap.width().to_le_bytes(), pixmap.height().to_le_bytes()].concat();
        Arc::new(Png {
            hash: hash_of(&[b"rgba", &dims, pixmap.data()]),
            pixels: Some(pixmap),
            bytes: std::sync::OnceLock::new(),
        })
    }
    /// The content hash: equal pixels, equal hash.
    pub fn hash(&self) -> u64 {
        self.hash
    }
    pub fn pixmap(&self) -> Option<&tiny_skia::Pixmap> {
        self.pixels.as_ref()
    }
    /// Whether the PNG bytes have been made yet.
    pub fn encoded(&self) -> bool {
        self.bytes.get().is_some()
    }
}
impl std::ops::Deref for Png {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.bytes.get_or_init(|| {
            self.pixels
                .as_ref()
                .and_then(|pm| super::raster::png(pm).ok())
                .map(Arc::from)
                .unwrap_or_else(|| Arc::from(Vec::new()))
        })
    }
}
impl PartialEq for Png {
    fn eq(&self, other: &Png) -> bool {
        self.hash == other.hash
    }
}
impl Eq for Png {}
impl std::fmt::Debug for Png {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Png({:016x})", self.hash)
    }
}
pub fn kitty(png: &[u8], row: usize, col: usize, cols: usize, rows: usize) -> String {
    transmit(png, row, col, cols, rows, None)
}
/// `kitty` under image id `id` (and placement id `id.1`, when given). `z=-1` keeps it under the
/// text: Warp paints a picture at z 0 or above in a layer of its own, and that layer takes the
/// click, so a tap on a card never arrives.
fn transmit(
    png: &[u8],
    row: usize,
    col: usize,
    cols: usize,
    rows: usize,
    id: Option<(usize, Option<usize>)>,
) -> String {
    let mut out = format!("\x1b[{};{}H", row + 1, col + 1);
    let id = id
        .map(|(id, p)| match p {
            Some(p) => format!("i={id},p={p},"),
            None => format!("i={id},"),
        })
        .unwrap_or_default();
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
/// raw pixels, and the pane stays blank until they arrive: a whole pane is about 4 MB. A band is
/// one row of that. A picture made from pixels is cut from its pixels: nothing is encoded or
/// decoded, and a band is encoded only if it has to be sent.
pub fn bands(pictures: &[Placement]) -> Vec<Placement> {
    let mut out = vec![];
    for p in pictures {
        if let Some(pm) = p.png.pixmap() {
            let (width, height) = (pm.width(), pm.height());
            if p.rows < 2 || height < p.rows as u32 {
                out.push(p.clone());
                continue;
            }
            let edge = |i: usize| (height as u64 * i as u64 / p.rows as u64) as u32;
            let stride = width as usize * 4;
            for i in 0..p.rows {
                let (top, bottom) = (edge(i), edge(i + 1));
                let data = pm.data()[top as usize * stride..bottom as usize * stride].to_vec();
                let Some(band) = tiny_skia::IntSize::from_wh(width, bottom - top)
                    .and_then(|size| tiny_skia::Pixmap::from_vec(data, size))
                else {
                    return pictures.to_vec();
                };
                out.push(Placement {
                    row: p.row + i,
                    col: p.col,
                    cols: p.cols,
                    rows: 1,
                    png: Png::from_pixmap(band),
                });
            }
            continue;
        }
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
                png: Png::from_bytes(png.into_inner()),
            });
        }
    }
    out
}
#[derive(Clone, Default)]
pub struct Drawn {
    pub cols: usize,
    pub rows: usize,
    pub written: Vec<String>,
    pub placed: Vec<Placement>,
    /// The image id each placed picture is on screen under; slot `i` is placement `i + 1` of it.
    pub ids: Vec<usize>,
    /// Images the terminal holds, by id and content hash, the longest unseen first. A band whose
    /// pixels come back (a page scrolled by one row) is placed again from these and not sent.
    pub held: Vec<(usize, u64)>,
}
/// Images kept in the terminal beyond those on screen, so a scroll back is placement-only too.
const HELD_SPARE: usize = 64;
/// Image ids run from 1 to 2^31 - 1: a picture's id comes from its content hash, the next free
/// one on a clash.
const ID_SPAN: u64 = 0x7fff_fffe;
fn id_for(hash: u64, taken: impl Fn(usize) -> bool) -> usize {
    let mut id = (hash % ID_SPAN) as usize + 1;
    while taken(id) {
        id = id % ID_SPAN as usize + 1;
    }
    id
}
fn same_size(drawn: Option<&Drawn>, cols: usize, rows: usize) -> Option<&Drawn> {
    drawn.filter(|d| d.cols == cols && d.rows == rows)
}
/// The image id of each picture after this frame (`repaint_frame`'s).
pub fn picture_ids(
    drawn: Option<&Drawn>,
    cols: usize,
    rows: usize,
    written: &[String],
    placed: &[Placement],
) -> Vec<usize> {
    repaint_frame(drawn, cols, rows, written, placed).1.ids
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
    repaint_frame(drawn, cols, rows, written, placed).0
}
/// What to write for this frame after `drawn`, and what is on screen once it is written.
///
/// A picture's image id comes from its pixels, so a band that only moved (a page scrolled by a
/// row) is placed again from the image the terminal holds (`a=p`), and only new pixels are sent.
/// herdr keys its own copy for the outer terminal by content too, so that hop sends nothing either.
/// Every new placement goes up before any old one is deleted, all inside one synchronized update:
/// herdr shows the gap between a delete and the next picture as a blank band.
pub fn repaint_frame(
    drawn: Option<&Drawn>,
    cols: usize,
    rows: usize,
    written: &[String],
    placed: &[Placement],
) -> (String, Drawn) {
    let same = same_size(drawn, cols, rows);
    let mut out = SYNC_ON.to_owned();
    let start = out.len();
    if same.is_none() {
        out.push_str(DELETE_ALL);
        out.push_str("\x1b[H\x1b[2J");
    }
    let mut held: Vec<(usize, u64)> = same.map(|d| d.held.clone()).unwrap_or_default();
    for (i, line) in written.iter().enumerate() {
        if same.and_then(|d| d.written.get(i)) != Some(line) {
            out.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[2K{line}", i + 1));
        }
    }
    let mut ids: Vec<usize> = Vec::with_capacity(placed.len());
    for (i, p) in placed.iter().enumerate() {
        let hash = p.png.hash();
        let id = held
            .iter()
            .find(|e| e.1 == hash)
            .map(|e| e.0)
            .or_else(|| {
                placed[..i]
                    .iter()
                    .zip(&ids)
                    .find(|(q, _)| q.png.hash() == hash)
                    .map(|(_, id)| *id)
            })
            .unwrap_or_else(|| {
                id_for(hash, |id| {
                    held.iter().any(|e| e.0 == id) || ids.contains(&id)
                })
            });
        ids.push(id);
    }
    let shown = same.map(|d| d.placed.as_slice()).unwrap_or_default();
    let shown_id = |i: usize| same.and_then(|d| d.ids.get(i).copied());
    let changed = |row: usize| same.and_then(|d| d.written.get(row)) != written.get(row);
    let under = |p: &Placement| (p.row..p.row + p.rows).any(changed);
    // New and moved pictures first: placed again when the terminal holds them, sent when not.
    for (i, p) in placed.iter().enumerate() {
        let old = shown.get(i);
        if shown_id(i) == Some(ids[i]) && old == Some(p) && !under(p) && !old.is_some_and(under) {
            continue;
        }
        let at = (ids[i], Some(i + 1));
        if held.iter().any(|e| e.0 == ids[i]) {
            out.push_str(&place(p, at));
        } else {
            out.push_str(&transmit(&p.png, p.row, p.col, p.cols, p.rows, Some(at)));
            held.push((ids[i], p.png.hash()));
        }
    }
    // Then the old placements go: a slot now showing another image, or no longer drawn. A slot
    // that keeps its image was placed again over itself, under the same placement id.
    for i in 0..shown.len() {
        let Some(old) = shown_id(i) else {
            continue;
        };
        if ids.get(i) != Some(&old) {
            out.push_str(&format!("\x1b_Ga=d,d=i,i={old},p={},q=2;\x1b\\", i + 1));
        }
    }
    // Images on screen are the most recently seen; past the spare, the longest unseen are freed.
    held.sort_by_key(|e| ids.contains(&e.0));
    let unseen = held.iter().filter(|e| !ids.contains(&e.0)).count();
    for (id, _) in held.drain(..unseen.saturating_sub(HELD_SPARE)) {
        out.push_str(&format!("\x1b_Ga=d,d=I,i={id},q=2;\x1b\\"));
    }
    let next = Drawn {
        cols,
        rows,
        written: written.to_vec(),
        placed: placed.to_vec(),
        ids,
        held,
    };
    if out.len() == start {
        // Nothing changed. Any output at all makes herdr place every picture again.
        return (String::new(), next);
    }
    out.push_str(&format!("\x1b[{rows};{cols}H{SYNC_OFF}"));
    (out, next)
}
/// Place image `at.0` again, as placement `at.1`, where `p` goes: no pixels are sent.
fn place(p: &Placement, at: (usize, Option<usize>)) -> String {
    let pid = at.1.map(|p| format!("p={p},")).unwrap_or_default();
    format!(
        "\x1b[{};{}H\x1b_Ga=p,q=2,C=1,i={},{pid}c={},r={},z=-1;\x1b\\",
        p.row + 1,
        p.col + 1,
        at.0,
        p.cols,
        p.rows
    )
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
            ..Default::default()
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
            ..Default::default()
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
    /// The id a frame put each picture under, read back from what it wrote.
    fn sent_ids(out: &str, action: &str) -> Vec<usize> {
        out.split("\x1b_G")
            .skip(1)
            .filter(|t| t.starts_with(action))
            .filter_map(|t| t.split("i=").nth(1)?.split(',').next()?.parse().ok())
            .collect()
    }
    #[test]
    fn test_pictures_are_sent_again_only_when_they_or_the_rows_under_them_change() {
        let picture = Placement {
            row: 1,
            col: 2,
            cols: 4,
            rows: 2,
            png: Png::from_bytes(b"png".as_slice()),
        };
        let written = ["a", "b", "c", "legend"].map(str::to_owned).to_vec();
        let (first, drawn) = repaint_frame(None, 10, 4, &written, std::slice::from_ref(&picture));
        let id = sent_ids(&first, "a=T")[0];
        assert!(first.contains(&format!("i={id},p=1,c=4,r=2,z=-1,")));
        let mut next = drawn.written.clone();
        next[0] = "A".into();
        assert!(
            !repaint_pictures(Some(&drawn), 10, 4, &next, std::slice::from_ref(&picture))
                .contains("\x1b_G")
        );
        next = drawn.written.clone();
        next[2] = "C".into();
        // A row under it was rewritten: placed again from the image the terminal holds, over
        // itself (same placement id), so nothing is sent and nothing deleted.
        let under = repaint_pictures(Some(&drawn), 10, 4, &next, std::slice::from_ref(&picture));
        assert_eq!(sent_ids(&under, "a=p"), [id]);
        assert!(!under.contains("a=T") && !under.contains("a=d"));
        let gone = repaint_pictures(Some(&drawn), 10, 4, &drawn.written, &[]);
        assert!(gone.contains(&format!("a=d,d=i,i={id},p=1,q=2;")) && !gone.contains("a=T"));
        let mut changed = picture;
        changed.png = Png::from_bytes(b"new".as_slice());
        let out = repaint_pictures(Some(&drawn), 10, 4, &drawn.written, &[changed]);
        let new_id = sent_ids(&out, "a=T")[0];
        assert_ne!(new_id, id);
        assert!(out.find("a=T").unwrap() < out.find(&format!("a=d,d=i,i={id},p=1,q=2;")).unwrap());
    }
    fn levels(width: u32, height: u32, bands: &[u8]) -> tiny_skia::Pixmap {
        let mut pm = tiny_skia::Pixmap::new(width, height).unwrap();
        let count = bands.len();
        for (y, row) in pm.data_mut().chunks_mut(width as usize * 4).enumerate() {
            let level = bands[y * count / height as usize];
            row.copy_from_slice(&[level, level, level, u8::MAX].repeat(width as usize));
        }
        pm
    }
    fn solid(width: u32, height: u32, bands: &[u8]) -> Arc<Png> {
        let img = levels(width, height, bands);
        Png::from_bytes(super::super::raster::png(&img).unwrap())
    }
    fn page(bands: &[u8]) -> Vec<Placement> {
        super::bands(&[Placement {
            row: 0,
            col: 0,
            cols: 4,
            rows: bands.len(),
            png: Png::from_pixmap(levels(8, 2 * bands.len() as u32, bands)),
        }])
    }
    #[test]
    fn a_picture_is_cut_into_one_band_per_cell_row() {
        let encoded = Placement {
            row: 2,
            col: 1,
            cols: 4,
            rows: 3,
            png: solid(8, 6, &[1u8, 4, 7]),
        };
        let pixels = Placement {
            png: Png::from_pixmap(levels(8, 6, &[1u8, 4, 7])),
            ..encoded.clone()
        };
        for whole in [encoded, pixels] {
            let cut = bands(std::slice::from_ref(&whole));
            assert_eq!(cut.len(), 3);
            for (i, band) in cut.iter().enumerate() {
                assert_eq!((band.row, band.col, band.cols, band.rows), (2 + i, 1, 4, 1));
                let img = image::load_from_memory(&band.png).unwrap().to_rgba8();
                assert_eq!(img.dimensions(), (8, 2));
                assert_eq!(img.get_pixel(0, 0).0[0], 1 + 3 * i as u8);
            }
        }
        // Cut from pixels, a band is not encoded until its bytes are asked for.
        let cut = page(&[1u8, 2, 3]);
        assert!(cut.iter().all(|b| !b.png.encoded()));
        assert_eq!(
            cut[0].png,
            page(&[1u8, 9, 9])[0].png,
            "equal pixels, equal picture"
        );
        assert_ne!(cut[0].png, cut[1].png);
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
        assert!(transmit(&png, 0, 0, 1, 1, Some((7, None))).contains(",i=7,c=1,r=1,z=-1,"));
        let band = &page(&[1u8, 2])[0];
        assert!(place(band, (7, Some(1))).contains("a=p,q=2,C=1,i=7,p=1,c=4,r=1,z=-1;"));
    }
    #[test]
    fn a_scroll_by_one_band_sends_one_band_and_places_the_rest_again() {
        let written = vec![String::new(); 5];
        let (_, drawn) = repaint_frame(None, 4, 5, &written, &page(&[1u8, 2, 3, 4]));
        let (out, next) = repaint_frame(Some(&drawn), 4, 5, &written, &page(&[2u8, 3, 4, 5]));
        // One new band; the other three were up already and only move.
        assert_eq!(out.matches("a=T").count(), 1);
        assert_eq!(out.matches("a=p").count(), 3);
        assert_eq!(next.ids[..3], drawn.ids[1..]);
        // herdr draws the gap between a delete and the next picture as a blank band: every
        // placement goes up before the first old one is deleted, and nothing clears the pane.
        let last_place = out.rfind("a=T").max(out.rfind("a=p")).unwrap();
        assert!(last_place < out.find("a=d").unwrap());
        assert_eq!(out.matches("a=d,d=i,").count(), 4);
        assert!(!out.contains(DELETE_ALL) && !out.contains("\x1b[2J") && !out.contains("d=I"));
        // Back up: every band is still held, so nothing is sent at all.
        let (back, _) = repaint_frame(Some(&next), 4, 5, &written, &page(&[1u8, 2, 3, 4]));
        assert_eq!(back.matches("a=T").count(), 0);
        assert_eq!(back.matches("a=p").count(), 4);
        // The same frame again writes nothing.
        let (_, again) = repaint_frame(Some(&next), 4, 5, &written, &page(&[2u8, 3, 4, 5]));
        assert_eq!(
            repaint_frame(Some(&again), 4, 5, &written, &page(&[2u8, 3, 4, 5])).0,
            ""
        );
        // Two slots with the same pixels share one image, sent once.
        let (twins, _) = repaint_frame(None, 4, 5, &written, &page(&[7u8, 7, 8]));
        assert_eq!(twins.matches("a=T").count(), 2);
        assert_eq!(twins.matches("a=p").count(), 1);
    }
    #[test]
    fn images_off_screen_are_freed_past_the_spare_longest_unseen_first() {
        let written = vec![String::new(); 3];
        let mut drawn: Option<Drawn> = None;
        let mut freed = vec![];
        for level in 0..(HELD_SPARE as u8 + 10) {
            let (out, next) =
                repaint_frame(drawn.as_ref(), 4, 3, &written, &page(&[level, level / 2]));
            freed.extend(sent_ids(&out, "a=d,d=I"));
            drawn = Some(next);
        }
        let drawn = drawn.unwrap();
        assert!(!freed.is_empty());
        assert!(drawn.held.len() <= HELD_SPARE + 2);
        assert!(drawn
            .ids
            .iter()
            .all(|id| drawn.held.iter().any(|e| e.0 == *id)));
        assert!(freed.iter().all(|id| !drawn.ids.contains(id)));
        // A clash takes the next free id.
        assert_eq!(id_for(4, |id| id == 5), 6);
        assert_eq!(id_for(ID_SPAN - 1, |id| id == ID_SPAN as usize), 1);
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
