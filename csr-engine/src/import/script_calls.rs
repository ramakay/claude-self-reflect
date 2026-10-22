//! Whether a Codex `exec` script calls a CSR tool.
//!
//! Codex code mode runs every tool from one `exec` call whose input is
//! JavaScript, and calls a CSR tool as `tools.mcp__claude_self_reflect__<tool>(...)`.
//! A script that only quotes that text (an `rg` pattern, a comment, a log line)
//! is not a call, so the script is tokenised and the call is looked for in the
//! code, never inside a string, template text, regular expression or comment.
//!
//! The tokenizer is deliberately small. Anything it cannot read with certainty
//! (an unterminated string or comment, a regular expression running off its
//! line) makes the answer "no call": a missed call leaves one tool result in
//! the corpus, where a wrong "call" would throw away a script's real output.

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Punct(char),
    Str(String),
    Other,
}

/// Whether `script` contains a call expression on a CSR tool in code position.
pub(crate) fn script_calls_csr_tool(script: &str) -> bool {
    let chars: Vec<char> = script.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0;
    if lex_code(&chars, &mut at, false, &mut tokens, 0).is_none() {
        return false;
    }
    tokens.windows(4).enumerate().any(|(i, window)| {
        let [Token::Ident(root), ..] = window else {
            return false;
        };
        if root != "tools" {
            return false;
        }
        match (&window[1], &window[2], &window[3]) {
            (Token::Punct('.'), Token::Ident(name), Token::Punct('(')) => is_csr_name(name),
            (Token::Punct('['), Token::Str(name), Token::Punct(']')) => {
                is_csr_name(name) && tokens.get(i + 4) == Some(&Token::Punct('('))
            }
            _ => false,
        }
    })
}

fn is_csr_name(name: &str) -> bool {
    let Some(tool) = name
        .strip_prefix("mcp__claude_self_reflect__")
        .or_else(|| name.strip_prefix("mcp__claude-self-reflect__"))
    else {
        return false;
    };
    !tool.is_empty() && tool.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Keywords after which a `/` opens a regular expression, not a division.
const REGEX_AFTER_KEYWORD: [&str; 9] = [
    "return", "typeof", "case", "in", "of", "await", "void", "yield", "delete",
];
const MAX_TEMPLATE_DEPTH: usize = 8;

/// Tokenise code from `*at`. With `until_brace` it stops after the `}` that
/// closes a template expression. `None` means the text could not be read.
fn lex_code(
    chars: &[char],
    at: &mut usize,
    until_brace: bool,
    out: &mut Vec<Token>,
    depth: usize,
) -> Option<()> {
    if depth > MAX_TEMPLATE_DEPTH {
        return None;
    }
    let mut braces = 0usize;
    while *at < chars.len() {
        let c = chars[*at];
        match c {
            c if c.is_whitespace() => *at += 1,
            '/' if chars.get(*at + 1) == Some(&'/') => {
                while *at < chars.len() && chars[*at] != '\n' {
                    *at += 1;
                }
            }
            '/' if chars.get(*at + 1) == Some(&'*') => {
                *at += 2;
                loop {
                    if *at + 1 >= chars.len() {
                        return None;
                    }
                    if chars[*at] == '*' && chars[*at + 1] == '/' {
                        *at += 2;
                        break;
                    }
                    *at += 1;
                }
            }
            '\'' | '"' => {
                let quote = c;
                *at += 1;
                let mut text = String::new();
                loop {
                    let current = *chars.get(*at)?;
                    *at += 1;
                    match current {
                        '\\' => {
                            text.push(*chars.get(*at)?);
                            *at += 1;
                        }
                        '\n' => return None,
                        current if current == quote => break,
                        current => text.push(current),
                    }
                }
                out.push(Token::Str(text));
            }
            '`' => {
                *at += 1;
                let mut text = String::new();
                let mut interpolated = false;
                loop {
                    let current = *chars.get(*at)?;
                    *at += 1;
                    match current {
                        '\\' => {
                            text.push(*chars.get(*at)?);
                            *at += 1;
                        }
                        '`' => break,
                        '$' if chars.get(*at) == Some(&'{') => {
                            *at += 1;
                            interpolated = true;
                            lex_code(chars, at, true, out, depth + 1)?;
                        }
                        current => text.push(current),
                    }
                }
                out.push(if interpolated {
                    Token::Other
                } else {
                    Token::Str(text)
                });
            }
            '/' => {
                let regex_allowed = match out.last() {
                    None => true,
                    Some(Token::Punct(p)) => !matches!(p, ')' | ']'),
                    Some(Token::Ident(word)) => REGEX_AFTER_KEYWORD.contains(&word.as_str()),
                    Some(Token::Str(_) | Token::Other) => false,
                };
                if regex_allowed {
                    *at += 1;
                    let mut in_class = false;
                    loop {
                        let current = *chars.get(*at)?;
                        *at += 1;
                        match current {
                            '\\' => *at += 1,
                            '\n' => return None,
                            '[' => in_class = true,
                            ']' => in_class = false,
                            '/' if !in_class => break,
                            _ => {}
                        }
                    }
                    while chars.get(*at).is_some_and(|f| f.is_ascii_alphabetic()) {
                        *at += 1;
                    }
                    out.push(Token::Other);
                } else {
                    *at += 1;
                    out.push(Token::Punct('/'));
                }
            }
            c if c.is_alphabetic() || c == '_' || c == '$' => {
                let start = *at;
                while chars
                    .get(*at)
                    .is_some_and(|n| n.is_alphanumeric() || *n == '_' || *n == '$')
                {
                    *at += 1;
                }
                // A hyphenated CSR server name, as the namespace is sometimes written.
                while chars[start..*at].starts_with(&['m', 'c', 'p', '_', '_'])
                    && chars.get(*at) == Some(&'-')
                    && chars.get(*at + 1).is_some_and(|n| n.is_alphanumeric())
                {
                    *at += 1;
                    while chars
                        .get(*at)
                        .is_some_and(|n| n.is_alphanumeric() || *n == '_')
                    {
                        *at += 1;
                    }
                }
                out.push(Token::Ident(chars[start..*at].iter().collect()));
            }
            c if c.is_ascii_digit() => {
                while chars
                    .get(*at)
                    .is_some_and(|n| n.is_alphanumeric() || *n == '.' || *n == '_')
                {
                    *at += 1;
                }
                out.push(Token::Other);
            }
            '{' => {
                braces += 1;
                *at += 1;
                out.push(Token::Punct('{'));
            }
            '}' => {
                *at += 1;
                if until_brace && braces == 0 {
                    return Some(());
                }
                braces = braces.saturating_sub(1);
                out.push(Token::Punct('}'));
            }
            c => {
                *at += 1;
                out.push(Token::Punct(c));
            }
        }
    }
    (!until_brace).then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_in_code_is_a_call() {
        for source in [
            "text(await tools.mcp__claude_self_reflect__csr_reflect_on_past({query:\"q\",limit:3}));",
            "const r = await tools.mcp__claude-self-reflect__csr_why({query:\"q\"});",
            "await tools[\"mcp__claude_self_reflect__csr_why\"]({query:\"q\"})",
            "const a = await tools.exec_command({cmd:\"ls\"});\nconst b = await tools.mcp__claude_self_reflect__csr_why ( {query: `why ${a}`} );",
            "/* probe */ x = 1 / 2; await tools.mcp__claude_self_reflect__get_timeline({})",
            "const q = `${await tools.mcp__claude_self_reflect__csr_why({query:'q'})}`;",
        ] {
            assert!(script_calls_csr_tool(source), "{source}");
        }
    }

    /// Finding 4: quoting the namespace is not calling it, whatever the quote.
    #[test]
    fn a_quoted_namespace_is_not_a_call() {
        for source in [
            "text(await tools.exec_command({cmd: \"rg -F 'await tools.mcp__claude_self_reflect__csr_why(' csr-engine/src\"}))",
            "text(await tools.exec_command({cmd: 'rg \"tools.mcp__claude_self_reflect__csr_why(\" src'}))",
            "// await tools.mcp__claude_self_reflect__csr_why({query:\"q\"})\ntext(await tools.exec_command({cmd:\"ls\"}));",
            "/* await tools.mcp__claude_self_reflect__csr_why({}) */ text(1)",
            "const note = `calls tools.mcp__claude_self_reflect__csr_why( later`;",
            "const re = /tools\\.mcp__claude_self_reflect__csr_why\\(/; text(re.test(s))",
            "text(await tools.exec_command({cmd:\"rg tools.mcp__claude_self_reflect__csr_why src\"}));",
            "const n = tools.mcp__claude_self_reflect__csr_why; text(n.name)",
        ] {
            assert!(!script_calls_csr_tool(source), "{source}");
        }
    }

    #[test]
    fn a_script_it_cannot_read_is_not_a_call() {
        for source in [
            "await tools.mcp__claude_self_reflect__csr_why({query:\"unterminated})",
            "/* open comment tools.mcp__claude_self_reflect__csr_why({})",
            "const x = `open ${ tools.mcp__claude_self_reflect__csr_why({}) ",
        ] {
            assert!(!script_calls_csr_tool(source), "{source}");
        }
    }
}
