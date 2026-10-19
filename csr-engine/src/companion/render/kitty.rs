pub const SYNC_ON: &str = "\x1b[?2026h";
pub const SYNC_OFF: &str = "\x1b[?2026l";
pub const DELETE_ALL: &str = "\x1b_Ga=d,d=A\x1b\\";
#[derive(Clone)]
pub struct Drawn {
    pub cols: usize,
    pub rows: usize,
    pub written: Vec<String>,
}
pub fn repaint(drawn: Option<&Drawn>, cols: usize, rows: usize, written: &[String]) -> String {
    let same = drawn.filter(|d| d.cols == cols && d.rows == rows);
    let mut out = SYNC_ON.to_owned();
    if same.is_none() {
        out.push_str(DELETE_ALL);
        out.push_str("\x1b[H\x1b[2J");
    }
    for (i, line) in written.iter().enumerate() {
        if same.and_then(|d| d.written.get(i)) != Some(line) {
            out.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[2K{line}", i + 1));
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
        };
        let b = repaint(Some(&d), 46, 2, &["a".into(), "c".into()]);
        assert_eq!(
            b,
            format!("{SYNC_ON}\x1b[2;1H\x1b[0m\x1b[2Kc\x1b[2;46H{SYNC_OFF}")
        );
        assert!(repaint(Some(&d), 47, 2, &d.written).contains("\x1b[2J"));
    }
}
