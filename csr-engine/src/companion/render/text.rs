use super::super::{
    array, masthead,
    pyfmt::{round, short},
    string,
};
use serde_json::Value;
pub const OFF: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub type Rgb = [u8; 3];
pub fn ink_rgb(name: &str) -> Rgb {
    match name {
        "you" => [23, 25, 28],
        "ctx" => [44, 90, 160],
        "learn" => [36, 131, 99],
        "turn" => [200, 68, 42],
        "dream" => [152, 108, 0],
        "recall" => [36, 131, 99],
        "asked" => [123, 95, 201],
        "sepia" => [111, 86, 66],
        "rule" => [0, 0, 0],
        "quiet" => [94, 94, 89],
        "faint" => [136, 135, 129],
        "hair" => [226, 225, 219],
        _ => [23, 25, 28],
    }
}
pub fn sev_rgb(name: &str) -> Rgb {
    match name {
        "normal" => [147, 155, 165],
        "data" => [238, 245, 250],
        "advisory" => [58, 127, 224],
        "amber" => [242, 182, 50],
        "red" => [232, 88, 44],
        _ => [23, 25, 28],
    }
}
pub fn section(name: &str) -> &'static str {
    match name {
        "session" => "you",
        "touching" => "ctx",
        "risk" => "turn",
        "rules" => "sepia",
        "dreams" => "dream",
        "memory" => "recall",
        "changed" => "quiet",
        "remembered" => "recall",
        "kept" => "turn",
        "story" => "ctx",
        "coach" => "ctx",
        "told" => "recall",
        "last" => "ctx",
        "todos" => "sepia",
        "compacted" => "ctx",
        "flight" => "ctx",
        "earlier" => "quiet",
        "lost" => "turn",
        _ => "",
    }
}
pub fn glyph(name: &str) -> &'static str {
    match name {
        "session" => "◉",
        "touching" => "‹›",
        "risk" => "~",
        "rules" => "◆",
        "dreams" => "☾",
        "memory" => "↑",
        "changed" => "~",
        "remembered" => "↑",
        "kept" => "◆",
        "story" => "§",
        "coach" => "→",
        "told" => "↑",
        "last" => "§",
        "todos" => "◆",
        "compacted" => "§",
        "flight" => "»",
        "earlier" => "↓",
        "lost" => "?",
        _ => "",
    }
}
pub fn fg(rgb: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", rgb[0], rgb[1], rgb[2])
}
pub fn bg(rgb: Rgb) -> String {
    format!("\x1b[48;2;{};{};{}m", rgb[0], rgb[1], rgb[2])
}
pub fn ink(name: &str) -> String {
    fg(ink_rgb(name))
}
pub fn slot(text: &str, sev: &str) -> String {
    format!("{}{text}{OFF}", fg(sev_rgb(sev)))
}
pub fn len(text: &str) -> usize {
    text.chars().count()
}
pub fn fit(text: &str, width: usize) -> String {
    if len(text) <= width {
        text.into()
    } else {
        format!(
            "{}…",
            text.chars()
                .take(width.saturating_sub(1).max(1))
                .collect::<String>()
        )
    }
}
pub fn wrap(text: &str, width: usize, rows: usize) -> Vec<String> {
    if rows == 0 {
        return vec![];
    }
    let mut lines = vec![];
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && len(&line) + 1 + len(word) > width {
            lines.push(line);
            line = String::new();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    if lines.len() > rows {
        lines.truncate(rows);
        lines[rows - 1].push_str(" …");
    }
    lines.iter().map(|s| fit(s, width)).collect()
}
pub fn fit_pieces(text: &str, width: usize) -> String {
    let mut pieces: Vec<_> = text.split(" · ").collect();
    while pieces.len() > 1 && len(&pieces.join(" · ")) > width {
        pieces.pop();
    }
    fit(&pieces.join(" · "), width)
}
pub fn plain(text: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap());
    RE.replace_all(text, "").into_owned()
}
pub fn ansi_fit(text: &str, width: usize) -> String {
    if len(&plain(text)) <= width {
        return text.into();
    }
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    let mut n = 0;
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            for c in chars.by_ref() {
                out.push(c);
                if c == 'm' {
                    break;
                }
            }
        } else {
            if n >= width.saturating_sub(1) {
                out.push('…');
                out.push_str(OFF);
                break;
            }
            out.push(c);
            n += 1;
        }
    }
    out
}
pub fn one_line(text: &str, most: usize) -> String {
    let clean: String = text.chars().filter(|c| !control(*c)).collect();
    fit(
        &clean.split_whitespace().collect::<Vec<_>>().join(" "),
        most,
    )
}
pub fn control(c: char) -> bool {
    matches!(c as u32,0..=8|11..=31|127..=159)
}
pub fn safe_line(text: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\x1b\[[0-9;:]*m|\x1b\[K").unwrap());
    let mut out = String::new();
    let mut at = 0;
    for m in RE.find_iter(text) {
        out.extend(text[at..m.start()].chars().filter(|c| !control(*c)));
        out.push_str(m.as_str());
        at = m.end();
    }
    out.extend(text[at..].chars().filter(|c| !control(*c)));
    out
}
pub fn tinted(text: &str, back: &str) -> String {
    format!(
        "{back}{}\x1b[K{OFF}",
        text.replace(OFF, &format!("{OFF}{back}"))
    )
}
pub fn on_dark(text: &str, ground: Rgb) -> String {
    let back = bg(ground);
    let fg = fg([238, 245, 250]);
    format!(
        "{back}{fg}{}\x1b[K{OFF}",
        text.replace(OFF, &format!("{OFF}{back}{fg}"))
    )
}
pub fn tint(name: &str, strength: f64) -> Rgb {
    let strength = strength * if name == "dreams" { 1.5 } else { 1. };
    let color = ink_rgb(section(name));
    std::array::from_fn(|i| {
        round([251., 251., 249.][i] + (color[i] as f64 - [251., 251., 249.][i]) * strength) as u8
    })
}
pub(super) fn linear(c: f64) -> f64 {
    let c = c / 255.;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
pub(super) fn lab(rgb: Rgb) -> [f64; 3] {
    let [r, g, b] = rgb.map(|n| linear(n as f64));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f64| {
        if t > 216. / 24389. {
            t.powf(1. / 3.)
        } else {
            (24389. / 27. * t + 16.) / 116.
        }
    };
    [
        116. * f(y) - 16.,
        500. * (f(x) - f(y)),
        200. * (f(y) - f(z)),
    ]
}
pub(super) fn from_lab([l, a, b]: [f64; 3]) -> Rgb {
    let fy = (l + 16.) / 116.;
    let g = |t: f64| {
        if t.powi(3) > 216. / 24389. {
            t.powi(3)
        } else {
            (116. * t - 16.) / (24389. / 27.)
        }
    };
    let [x, y, z] = [
        g(fy + a / 500.) * 0.95047,
        g(fy),
        g(fy - b / 200.) * 1.08883,
    ];
    let enc = |c: f64| {
        round(
            (if c <= 0.0031308 {
                c * 12.92
            } else {
                1.055 * c.max(0.).powf(1. / 2.4) - 0.055
            }) * 255.,
        )
        .clamp(0., 255.) as u8
    };
    [
        enc(3.2406 * x - 1.5372 * y - 0.4986 * z),
        enc(-0.9689 * x + 1.8758 * y + 0.0415 * z),
        enc(0.0557 * x - 0.2040 * y + 1.0570 * z),
    ]
}
pub fn blend(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let a = lab(a);
    let b = lab(b);
    from_lab(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t))
}
pub fn dark_steps(n: usize) -> Vec<Rgb> {
    (0..n)
        .map(|i| {
            blend(
                [47, 58, 72],
                [14, 15, 17],
                i as f64 / n.saturating_sub(1).max(1) as f64,
            )
        })
        .collect()
}
pub fn heat(tokens: f64) -> Rgb {
    let v = if tokens.is_finite() { tokens } else { 0. };
    let amber = masthead::ctx_amber();
    let hot = masthead::ctx_hot().max(amber);
    let red = masthead::ctx_red().max(hot);
    let v = (v.clamp(0., red) / 1000.).floor() * 1000.;
    if v <= amber {
        blend([58, 127, 224], [64, 190, 176], v / amber)
    } else if v <= hot {
        blend([64, 190, 176], [242, 182, 50], (v - amber) / (hot - amber))
    } else {
        blend([242, 182, 50], [232, 88, 44], (v - hot) / (red - hot))
    }
}
pub const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
pub fn spark(values: &Value, n: usize) -> String {
    let values: Vec<_> = array(values)
        .iter()
        .filter_map(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.)
        .collect();
    let values = &values[values.len().saturating_sub(n)..];
    let mut out = " ".repeat(n - values.len());
    for v in values {
        out.push_str(&format!(
            "{}{}{OFF}",
            fg(heat(*v)),
            EIGHTHS[(round((v / masthead::ctx_red()).min(1.) * 8.) as usize).clamp(1, 8)]
        ));
    }
    out
}
pub fn ctx_row(g: &Value, width: usize) -> String {
    let ctx = &g["ctx"];
    let tokens = ctx["tokens"].as_f64();
    let size = ctx["size"].as_f64();
    let trend = g["trend"].as_f64();
    let arrow = trend
        .filter(|t| t.abs() >= 1000.)
        .map(|t| {
            format!(
                "{}{}{}",
                if t > 0. { "▲" } else { "▼" },
                short(t.abs()),
                "/t"
            )
        })
        .unwrap_or_default();
    let values = match (tokens, size) {
        (None, _) => vec!["--".into()],
        (Some(t), Some(s)) => vec![format!("{}/{}", short(t), short(s)), short(t)],
        (Some(t), _) => vec![short(t)],
    };
    let pay = &g["payback"];
    let pays = if g["compacting"].is_object() {
        [(true, true), (true, false), (false, true)]
            .into_iter()
            .map(|(bar, range)| masthead::compacting_slot(&g["compacting"], bar, range).0)
            .collect::<Vec<_>>()
    } else if array(&pay["forms"]).is_empty() {
        vec![string(&pay["text"]).to_owned()]
    } else {
        array(&pay["forms"])
            .iter()
            .map(|v| string(v).to_owned())
            .collect()
    };
    let mut tries = vec![
        (values[0].clone(), arrow, pays[0].clone(), true),
        (values[0].clone(), String::new(), pays[0].clone(), true),
        (values[0].clone(), String::new(), pays[0].clone(), false),
    ];
    for pay in &pays[1..] {
        for want in [true, false] {
            tries.push((values[0].clone(), String::new(), pay.clone(), want));
        }
    }
    for pay in &pays {
        tries.push((
            values.last().unwrap().clone(),
            String::new(),
            pay.clone(),
            false,
        ));
    }
    let sev = if tokens.is_none() {
        "normal"
    } else if ctx["sev"] == "normal" {
        "data"
    } else {
        string(&ctx["sev"])
    };
    for (value, tail, text, want) in tries {
        let fixed = 5
            + len(&value)
            + if tail.is_empty() { 0 } else { 1 + len(&tail) }
            + if text.is_empty() { 0 } else { 2 + len(&text) };
        let turns = !array(&g["turns"]).is_empty();
        let n = if want && turns {
            24.min(width.saturating_sub(fixed + 1))
        } else {
            0
        };
        if (want && turns && n < 6) || fixed + if n > 0 { n + 1 } else { 0 } > width {
            continue;
        }
        return format!(
            " {}{}{}{}{}",
            slot("CTX ", "normal"),
            if n > 0 {
                format!("{} ", spark(&g["turns"], n))
            } else {
                String::new()
            },
            slot(&value, sev),
            if tail.is_empty() {
                String::new()
            } else {
                format!(" {}", slot(&tail, "normal"))
            },
            if text.is_empty() {
                String::new()
            } else {
                format!("  {}", slot(&text, string(&pay["sev"])))
            }
        );
    }
    format!(
        " {}{}",
        slot("CTX ", "normal"),
        slot(
            &fit(values.last().unwrap(), width.saturating_sub(5).max(1)),
            sev
        )
    )
}
pub fn ahead_of_pace(q: &Value) -> bool {
    q["used"]
        .as_f64()
        .zip(q["pace"].as_f64())
        .is_some_and(|(used, pace)| used / 100. > pace + 0.05)
}
pub fn paced(q: &Value) -> String {
    let t = string(&q["text"]);
    if !ahead_of_pace(q) {
        return t.into();
    }
    let (head, tail) = t.split_once(" ↻").unwrap_or((t, ""));
    format!(
        "{head}»{}",
        if tail.is_empty() {
            String::new()
        } else {
            format!(" ↻{tail}")
        }
    )
}
pub fn status_row(g: &Value, width: usize) -> String {
    let q = array(&g["quotas"]);
    let sts = string(&g["sts"]["sev"]);
    let mark = if sts.is_empty() { "" } else { "STS" };
    let reserve = if mark.is_empty() { 0 } else { 4 };
    let full: Vec<_> = q
        .iter()
        .map(|q| (paced(q), string(&q["sev"]).to_owned()))
        .collect();
    let head = full[0].0.split(" ↻").next().unwrap().to_owned();
    let bare = head
        .split_once(' ')
        .map(|(_, v)| v)
        .unwrap_or(&head)
        .to_owned();
    let first = (head, full[0].1.clone());
    let cache = &g["cache"];
    let word = (
        string(&cache["text"]).to_owned(),
        string(&cache["sev"]).to_owned(),
    );
    let glyph = (
        format!("◷{}", word.0.strip_prefix("CACHE ").unwrap_or(&word.0)),
        word.1.clone(),
    );
    let tries = [
        vec![full[0].clone(), full[1].clone(), word],
        vec![full[0].clone(), full[1].clone(), glyph.clone()],
        vec![full[0].clone(), glyph.clone()],
        vec![first.clone(), glyph],
        vec![full[0].clone()],
        vec![first.clone()],
        vec![(bare.clone(), first.1.clone())],
    ];
    let parts = tries
        .into_iter()
        .find(|p| {
            1 + p.iter().map(|(t, _)| len(t)).sum::<usize>()
                + 2 * p.len().saturating_sub(1)
                + reserve
                <= width
        })
        .unwrap_or_else(|| {
            let tail = if bare.ends_with('»') { "»" } else { "" };
            vec![(
                format!(
                    "{}{tail}",
                    fit(
                        bare.trim_end_matches('»'),
                        width.saturating_sub(1 + reserve + len(tail)).max(1)
                    )
                ),
                first.1,
            )]
        });
    let used =
        1 + parts.iter().map(|(t, _)| len(t)).sum::<usize>() + 2 * parts.len().saturating_sub(1);
    let line = format!(
        " {}",
        parts
            .iter()
            .map(|(t, s)| slot(t, s))
            .collect::<Vec<_>>()
            .join("  ")
    );
    if mark.is_empty() {
        line
    } else {
        format!(
            "{line}{}{}",
            " ".repeat(width.saturating_sub(used + 3).max(1)),
            slot(mark, sts)
        )
    }
}
pub fn cockpit(g: &Value, width: usize) -> Vec<String> {
    vec![ctx_row(g, width), status_row(g, width)]
}
pub fn fill_levels(share: f64, height: usize) -> Vec<usize> {
    let levels = round(share.clamp(0., 1.) * (height * 8) as f64) as usize;
    (0..height)
        .map(|i| levels.saturating_sub((height - i - 1) * 8).min(8))
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn cockpit_fixture() -> Value {
        let now = 1792320000.;
        let fields = json!({
            "context_window": {
                "total_input_tokens": 87724,
                "context_window_size": 1000000,
            },
            "rate_limits": {
                "five_hour": {
                    "used_percentage": 95,
                    "resets_at": now + 7200.,
                },
                "seven_day": {
                    "used_percentage": 75,
                    "resets_at": now + 259200.,
                },
            },
        });
        let main: Vec<_> = (1..5)
            .map(|k| json!([105 + 100 * k, 60000 + 12000 * k]))
            .collect();
        masthead::gauges(
            &fields,
            Some(now),
            &json!({
                "main": main,
                "prompts": [200, 300, 400, 500],
            }),
            now,
            &Value::Null,
            &json!({
                "sev": "amber",
                "inop": [],
            }),
        )
    }
    #[test]
    fn test_rows_that_cannot_hold_everything_shrink_in_a_fixed_order() {
        let g = cockpit_fixture();
        assert_eq!(
            plain(&ctx_row(&g, 60)),
            format!(" CTX {}▁▂▂▂ 88k/1M ▲12k/t", " ".repeat(20))
        );
        assert_eq!(plain(&ctx_row(&g, 21)), " CTX      ▁▂▂▂ 88k/1M");
        assert_eq!(plain(&ctx_row(&g, 17)), " CTX 88k/1M");
        let big = masthead::gauges(
            &json!({
                "context_window": {
                    "total_input_tokens": 1200000,
                    "context_window_size": 10000000,
                },
            }),
            None,
            &Value::Null,
            1792320000.,
            &Value::Null,
            &Value::Null,
        );
        for (width, want) in [
            (40, " CTX 1.2M/10M  ♻ ~3 calls"),
            (21, " CTX 1.2M  ♻ ~3 calls"),
            (18, " CTX 1.2M"),
        ] {
            assert_eq!(plain(&ctx_row(&big, width)), want);
        }
        let first = paced(&g["quotas"][0]);
        let second = paced(&g["quotas"][1]);
        let full = 1 + len(&first) + 2 + len(&second) + 2 + len("CACHE --") + len(" STS");
        assert_eq!(
            plain(&status_row(&g, full)),
            format!(" {first}  {second}  CACHE -- STS")
        );
        assert_eq!(
            plain(&status_row(&g, full - 1)),
            format!(" {first}  {second}  ◷--     STS")
        );
        let glyph = 1 + len(&first) + 2 + len("◷--") + len(" STS");
        assert_eq!(plain(&status_row(&g, glyph)), format!(" {first}  ◷-- STS"));
        assert_eq!(
            plain(&status_row(&g, glyph - 1)),
            format!(" 5H 95%»  ◷--{}STS", " ".repeat(glyph - 17))
        );
        assert_eq!(plain(&status_row(&g, 16)), " 5H 95%»     STS");
        assert_eq!(plain(&status_row(&g, 11)), " 95%»   STS");
    }
    #[test]
    fn test_the_pace_mark_outlasts_every_other_piece_of_row_two() {
        let mut g = cockpit_fixture();
        for sts in [
            json!({
                "sev": "amber",
            }),
            Value::Null,
        ] {
            g["sts"] = sts;
            for width in 9..70 {
                let row = plain(&status_row(&g, width));
                assert!(len(&row) <= width, "{width}: {row}");
                assert!(row.contains("95%»"), "{width}: {row}");
            }
        }
    }
    #[test]
    fn test_a_dark_cockpit_lights_only_what_deviates() {
        let now = 1792320000.;
        for (cache, want, sev) in [
            (
                json!({
                    "warm": true,
                    "ttl": "1h",
                    "expires_at": now + 2480.,
                }),
                "CACHE 41m",
                "normal",
            ),
            (
                json!({
                    "warm": true,
                    "ttl": "1h",
                    "expires_at": now + 250.,
                }),
                "CACHE 4m",
                "amber",
            ),
            (
                json!({
                    "warm": true,
                    "ttl": "1h",
                    "expires_at": now + 30.,
                }),
                "CACHE <1m",
                "amber",
            ),
            (
                json!({
                    "warm": false,
                    "expires_at": now - 10.,
                    "recache_tokens_if_cold": 152300,
                }),
                "CACHE COLD ~152k",
                "amber",
            ),
            (Value::Null, "CACHE --", "normal"),
            (
                json!({
                    "warm": true,
                    "ttl": "5m",
                }),
                "CACHE 5m",
                "normal",
            ),
        ] {
            let g = masthead::gauges(
                &json!({
                    "prompt_cache": cache,
                }),
                Some(now),
                &Value::Null,
                now,
                &Value::Null,
                &Value::Null,
            );
            assert!(status_row(&g, 100).contains(&slot(want, sev)));
        }
    }
    #[test]
    fn test_a_phone_draws_the_same_two_rows() {
        let g = masthead::gauges(
            &json!({
                "context_window": {
                    "total_input_tokens": 155000,
                },
            }),
            None,
            &Value::Null,
            10000.,
            &Value::Null,
            &Value::Null,
        );
        for width in 10..70 {
            for line in cockpit(&g, width) {
                assert!(len(&plain(&line)) <= width, "{width}: {line:?}");
            }
        }
    }
    #[test]
    fn test_no_terminal_controls_reach_a_prompt() {
        assert_eq!(one_line("a\x1b]52;c;x\x07\nb", 100), "a]52;c;x b");
        assert!(!safe_line("evil\x1b[2J").contains('\x1b'));
        assert!(safe_line("\x1b[31mred\x1b[0m").contains("\x1b[31m"));
    }
    #[test]
    fn test_the_masthead_steps_down_from_payne_to_night() {
        assert_eq!(dark_steps(2), vec![[47, 58, 72], [14, 15, 17]]);
    }
    #[test]
    fn test_the_sparkline_is_the_last_turns_in_eighths_newest_at_the_right() {
        assert_eq!(
            plain(&spark(&json!([50000, 200000, 400000, 900000]), 6)),
            "  ▁▄██"
        );
    }
}
