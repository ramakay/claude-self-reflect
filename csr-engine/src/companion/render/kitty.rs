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
/// A picture's id follows its bytes, so an unchanged picture keeps its id across frames.
pub fn image_id(png: &[u8]) -> u32 {
    let hash = blake3::hash(png);
    let bytes = hash.as_bytes();
    (u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) & 0x7fff_ffff).max(1)
}
pub fn kitty(png: &[u8], row: usize, col: usize, cols: usize, rows: usize) -> String {
    let mut out = format!("\x1b[{};{}H", row + 1, col + 1);
    let data = STANDARD.encode(png);
    let chunks = data.as_bytes().chunks(4096);
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let more = usize::from(index + 1 < count);
        let ctrl = if index == 0 {
            format!(
                "a=T,f=100,q=2,C=1,i={},c={cols},r={rows},m={more}",
                image_id(png)
            )
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
    let again = placed != shown
        || changed.iter().any(|i| {
            shown
                .iter()
                .chain(placed)
                .any(|p| *i >= p.row && *i < p.row + p.rows)
        });
    for (i, line) in written.iter().enumerate() {
        if same.and_then(|d| d.written.get(i)) != Some(line) {
            out.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[2K{line}", i + 1));
        }
    }
    if again {
        // Place the new picture before freeing the old one: herdr draws the gap between a
        // delete and the next upload as a blank frame.
        for p in placed {
            out.push_str(&kitty(&p.png, p.row, p.col, p.cols, p.rows));
        }
        let kept: Vec<_> = placed.iter().map(|p| image_id(&p.png)).collect();
        for p in shown {
            let id = image_id(&p.png);
            if !kept.contains(&id) {
                out.push_str(&format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\"));
            }
        }
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
        assert_eq!(
            repaint(Some(&drawn), 10, 4, &drawn.written),
            format!("{SYNC_ON}\x1b[4;10H{SYNC_OFF}")
        );
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
        assert!(!under.contains(DELETE_ALL) && under.contains("a=T"));
        let gone = repaint_pictures(Some(&drawn), 10, 4, &drawn.written, &[]);
        assert!(gone.contains("a=d,d=I") && !gone.contains("a=T"));
        let mut changed = picture;
        changed.png = Arc::from(b"new".as_slice());
        assert!(repaint_pictures(Some(&drawn), 10, 4, &drawn.written, &[changed]).contains("a=T"));
    }
    #[test]
    fn a_changed_picture_is_placed_before_the_old_one_is_deleted() {
        let picture = |png: &'static [u8]| Placement {
            row: 0,
            col: 0,
            cols: 4,
            rows: 3,
            png: Arc::from(png),
        };
        let written = vec![String::new(); 4];
        let drawn = Drawn {
            cols: 4,
            rows: 4,
            written: written.clone(),
            placed: vec![picture(b"old")],
        };
        let out = repaint_pictures(Some(&drawn), 4, 4, &written, &[picture(b"new")]);
        // herdr shows the gap between a delete and the next upload as a blank frame.
        assert!(!out.contains(DELETE_ALL));
        let new = out
            .find(&format!("i={}", image_id(b"new")))
            .expect("new picture sent");
        let old = out
            .find(&format!("a=d,d=I,i={}", image_id(b"old")))
            .expect("old picture freed");
        assert!(new < old);
        let gone = repaint_pictures(Some(&drawn), 4, 4, &written, &[]);
        assert!(gone.contains(&format!("a=d,d=I,i={}", image_id(b"old"))));
    }
    #[test]
    fn kitty_chunks_transmit_png_and_place_without_moving_the_cursor() {
        let png = vec![42; 7000];
        let output = kitty(&png, 2, 3, 40, 5);
        assert!(output.starts_with(&format!(
            "\x1b[3;4H\x1b_Ga=T,f=100,q=2,C=1,i={},c=40,r=5,m=1;",
            image_id(&png)
        )));
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
