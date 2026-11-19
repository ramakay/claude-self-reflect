//! Is a prompt something the person asked, or something they pasted? The pane quotes a person's
//! last prompt back to them ("Last asked before it ended: …"). A pasted router status page
//! (`Firmware=V7.2.8.8_5.1.24 RegionTag=RBRE960_NA …`, seen on a clean install), a JSON body, a
//! log or a hash is not a question anyone wants read back: the picker skips it and takes the last
//! real one, or shows nothing.
use std::sync::LazyLock;

static KEY_VALUE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"^[A-Za-z_][\w.\-]*[=:][^\s=]*$"#).unwrap());
static LOG_LINE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r#"^\s*(?:\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}|\d{2}:\d{2}:\d{2}|\[?(?:TRACE|DEBUG|INFO|WARN"#,
        r#"|WARNING|ERROR|FATAL)\]?[\s:]|at\s+\S+\s*\(|\s*File ".*", line \d+)"#,
    ))
    .unwrap()
});
/// A word as people write one, in any script: letters and the marks that join them (Devanagari
/// vowel signs, Vietnamese tones), maybe an apostrophe or a hyphen, maybe closing punctuation
/// (Latin, the Devanagari danda, CJK full stops and commas).
static WORD: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"^[("“'«]?\p{L}[\p{L}\p{M}'’\-]*[)"”'»]?[.,;:!?।॥。、，？！]*$"#).unwrap()
});
/// A character of a script written without spaces between words: a sentence in it is one long
/// token, not a blob.
static UNSPACED: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"[\p{Han}\p{Hiragana}\p{Katakana}\p{Thai}\p{Lao}\p{Khmer}\p{Myanmar}]")
        .unwrap()
});

/// True when most of `token`'s characters come from a script written without spaces.
fn unspaced_run(token: &str) -> bool {
    UNSPACED.find_iter(token).count() * 2 > token.chars().count()
}

/// True when `text` reads as pasted data, not as something a person asked.
pub fn is_paste(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    // JSON (or the start of it): an object or array, or `"key":` pairs throughout.
    if (text.starts_with('{') || text.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(text).is_ok()
    {
        return true;
    }
    if text.matches("\":").count() >= 3 {
        return true;
    }
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() >= 2 && lines.iter().filter(|l| LOG_LINE.is_match(l)).count() * 2 >= lines.len()
    {
        return true;
    }
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let count = tokens.len();
    // key=value runs: three or more, and most of what was typed.
    let pairs = tokens
        .iter()
        .filter(|t| KEY_VALUE.is_match(t) && t.contains('='))
        .count();
    if pairs >= 3 && pairs * 2 >= count {
        return true;
    }
    // Long tokens without spaces (hashes, base64, minified code) make up most of it. A link is
    // something people type around their question, and a Japanese, Chinese or Thai sentence is
    // one token because those scripts put no spaces between words.
    let long: usize = tokens
        .iter()
        .filter(|t| t.chars().count() > 40 && !t.contains("://") && !unspaced_run(t))
        .map(|t| t.len())
        .sum();
    if long * 2 > text.len() {
        return true;
    }
    // Mostly not words.
    let words = tokens.iter().filter(|t| WORD.is_match(t)).count();
    count >= 6 && words * 5 < count * 2
}

#[cfg(test)]
mod tests {
    use super::is_paste;

    /// The prompt the clean Studio install quoted back as its last question.
    const ROUTER_STATUS: &str = "Firmware=V7.2.8.8_5.1.24 RegionTag=RBRE960_NA Region=us \
        Model=RBRE960 InternetConnectionStatus=Up SOAPVersion=3.46 XCloudSupported=1 \
        LoginMethod=2.0 DeviceMode=0 isBlankState=0 SOAP_HTTPs_Port=443";

    #[test]
    fn pasted_data_is_a_paste() {
        for paste in [
            ROUTER_STATUS,
            r#"{"completion_status":"partial","confidence":"MEDIUM","frameworks":[]}"#,
            r#"[{"id": 1, "name": "a"}, {"id": 2, "name": "b"}]"#,
            r#""status": "ok", "count": 3, "items": "x", "next": null"#,
            "2026-11-19 10:01:33 INFO daemon started\n2026-11-19 10:01:34 WARN slow import\n\
             2026-11-19 10:01:35 ERROR socket closed",
            "Traceback (most recent call last):\n  File \"a.py\", line 3, in <module>\n  \
             File \"b.py\", line 9, in f",
            "sha256:3b96d46672ac15dc9fbc62ac7ec4d12431706e8dc5e294ac31abf7ce0ca57f7c",
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N",
            "0x7f 0x45 0x4c 0x46 0x02 0x01 0x01 0x00 0x00 0x00 | 0x00 0x00",
            "PATH=/usr/bin HOME=/Users/x SHELL=/bin/zsh TERM=xterm-256color",
        ] {
            assert!(is_paste(paste), "{paste}");
        }
    }

    #[test]
    fn questions_people_type_are_not_pastes() {
        for asked in [
            "what did we work on lately?",
            "fix the bug in src/main.rs where retries=3 never fires",
            "Why does `cargo test --lib` fail on the Studio but not here?",
            "check https://github.com/ramakay/claude-self-reflect/pull/327 and tell me what's left",
            "set CSR_COMPANION_PIXELS=0 and see if the pane draws",
            "ok",
            "can you look at this error: connection refused on port 443",
            "rename the field to snake_case and update the three callers",
        ] {
            assert!(!is_paste(asked), "{asked}");
        }
    }

    /// Words are letters in any script, not only A to Z, and a sentence in a script written
    /// without spaces is not a hash. Before, every Russian or Hindi question read as "mostly not
    /// words" and a Japanese one as a 60-character blob.
    #[test]
    fn questions_in_other_languages_are_not_pastes() {
        let japanese =
            "このファイルのバグを直して、テストをもう一度実行してから結果を教えてください。\
                        変更点も短くまとめて、ドキュメントの説明も新しい動作に合わせてください。";
        assert!(japanese.chars().count() >= 60);
        for asked in [
            "почему тесты падают на студии, а здесь нет?",
            "कृपया इस फ़ाइल में बग ठीक करें और टेस्ट चलाएँ।",
            "sửa lỗi trong tệp này và chạy lại kiểm thử nhé",
            japanese,
            "请帮我修复这个文件里的错误，然后重新运行全部测试，再把结果和改动的地方都告诉我一下。",
            "ช่วยแก้บั๊กในไฟล์นี้แล้วรันเทสต์ใหม่อีกครั้งหนึ่งแล้วบอกผลลัพธ์ให้ฉันทราบด้วยนะครับ",
        ] {
            assert!(!is_paste(asked), "{asked}");
        }
        // Pasted data stays a paste, and one CJK character does not excuse a blob.
        for paste in [
            ROUTER_STATUS,
            r#"{"completion_status":"partial","confidence":"MEDIUM","frameworks":[]}"#,
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.漢dozjgNryP4J3jVmNHl0w5N",
        ] {
            assert!(is_paste(paste), "{paste}");
        }
    }
}
