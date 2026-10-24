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
pub fn cell_size() -> (u32, u32) {
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
    let same = drawn.filter(|d| d.cols == cols && d.rows == rows);
    let mut out = SYNC_ON.to_owned();
    let start = out.len();
    if same.is_none() {
        out.push_str(DELETE_ALL);
        out.push_str("\x1b[H\x1b[2J");
    }
    let shown = same.map(|d| d.placed.as_slice()).unwrap_or_default();
    let changed: Vec<_> = written
        .iter()
        .enumerate()
        .filter(|(i, l)| same.and_then(|d| d.written.get(*i)) != Some(*l))
        .map(|(i, _)| i)
        .collect();
    let under = |p: &Placement| changed.iter().any(|i| *i >= p.row && *i < p.row + p.rows);
    // Picture `i` keeps image id `i + 1`. A slot that moved, or is no longer drawn, is deleted.
    let place = |p: &Placement| (p.row, p.col, p.cols, p.rows);
    for (i, old) in shown.iter().enumerate() {
        if placed.get(i).is_none_or(|p| place(p) != place(old)) {
            out.push_str(&format!("\x1b_Ga=d,d=I,i={},q=2;\x1b\\", i + 1));
        }
    }
    for (i, line) in written.iter().enumerate() {
        if same.and_then(|d| d.written.get(i)) != Some(line) {
            out.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[2K{line}", i + 1));
        }
    }
    for (i, p) in placed.iter().enumerate() {
        let old = shown.get(i);
        if old != Some(p) || under(p) || old.is_some_and(under) {
            out.push_str(&transmit(&p.png, p.row, p.col, p.cols, p.rows, Some(i + 1)));
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
        assert!(!under.contains("a=d") && under.contains("a=T,f=100,q=2,C=1,i=1,"));
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
    fn only_the_band_that_changed_is_sent_again_and_nothing_is_deleted_first() {
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
        };
        let out = repaint_pictures(Some(&drawn), 4, 4, &written, &after);
        // herdr draws the gap between a delete and the next upload as a blank pane.
        assert!(!out.contains("a=d"));
        assert_eq!(out.matches("a=T").count(), 1);
        assert!(out.contains("a=T,f=100,q=2,C=1,i=2,"));
        assert_eq!(repaint_pictures(Some(&drawn), 4, 4, &written, &before), "");
        let gone = repaint_pictures(Some(&drawn), 4, 4, &written, &after[..1]);
        assert!(gone.contains("a=d,d=I,i=2,q=2;") && gone.contains("a=d,d=I,i=3,q=2;"));
        assert!(!gone.contains(DELETE_ALL));
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
