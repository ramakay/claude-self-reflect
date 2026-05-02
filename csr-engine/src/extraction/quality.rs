//! Quality pattern analysis using ast-grep-core.
//!
//! Detects anti-patterns and quality issues in code snippets.
//! Can analyze code from conversations or files directly via CLI.

use std::path::Path;

use ast_grep_core::matcher::KindMatcher;
use ast_grep_core::tree_sitter::LanguageExt;
use ast_grep_language::SupportLang;
use serde::{Deserialize, Serialize};

/// A quality finding with severity and suggestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityFinding {
    pub rule: String,
    pub severity: Severity,
    pub message: String,
    pub line: usize,
    pub snippet: String,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Error => write!(f, "ERROR"),
            Severity::Warning => write!(f, "WARN"),
            Severity::Info => write!(f, "INFO"),
        }
    }
}

/// Quality report for a file or code snippet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityReport {
    pub path: String,
    pub language: String,
    pub findings: Vec<QualityFinding>,
    pub score: f32,
}

impl QualityReport {
    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Quality Report: {} ({})\n",
            self.path, self.language
        ));
        out.push_str(&format!("Score: {:.0}/100\n", self.score));
        out.push_str(&format!("Findings: {}\n\n", self.findings.len()));

        for f in &self.findings {
            out.push_str(&format!(
                "  [{}] {} (line {})\n    {}\n",
                f.severity, f.rule, f.line, f.message
            ));
            if let Some(ref sug) = f.suggestion {
                out.push_str(&format!("    Suggestion: {}\n", sug));
            }
            out.push('\n');
        }

        out
    }
}

/// Analyze a file and return a quality report.
pub fn analyze_file(path: &Path) -> anyhow::Result<QualityReport> {
    let source = std::fs::read_to_string(path)?;
    let lang = super::ast_analysis::lang_from_path_str(&path.to_string_lossy())
        .ok_or_else(|| anyhow::anyhow!("Unsupported language for: {}", path.display()))?;

    Ok(analyze_source(&source, lang, &path.to_string_lossy()))
}

/// Analyze a source code string.
pub fn analyze_source(source: &str, lang: SupportLang, path: &str) -> QualityReport {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        analyze_source_inner(source, lang, path)
    }));

    result.unwrap_or_else(|_| QualityReport {
        path: path.to_string(),
        language: lang_name(lang).to_string(),
        findings: vec![QualityFinding {
            rule: "parse_error".to_string(),
            severity: Severity::Warning,
            message: "Failed to parse file".to_string(),
            line: 0,
            snippet: String::new(),
            suggestion: None,
        }],
        score: 50.0,
    })
}

fn analyze_source_inner(source: &str, lang: SupportLang, path: &str) -> QualityReport {
    let mut findings = Vec::new();

    let grep = lang.ast_grep(source);
    let root = grep.root();

    match lang {
        SupportLang::Rust => {
            // R1: .unwrap() without context → suggest expect("reason") or ?
            let unwrap_matcher = KindMatcher::new("call_expression", lang);
            for node in root.find_all(&unwrap_matcher) {
                let text = node.text();
                if text.ends_with(".unwrap()") && !text.contains("test") {
                    findings.push(QualityFinding {
                        rule: "rust-unwrap".to_string(),
                        severity: Severity::Warning,
                        message: ".unwrap() without context".to_string(),
                        line: node.start_pos().line() + 1,
                        snippet: truncate_text(&text, 80),
                        suggestion: Some("Use .expect(\"reason\") or ? operator".to_string()),
                    });
                }
            }

            // R2: unsafe blocks
            let unsafe_matcher = KindMatcher::new("unsafe_block", lang);
            for node in root.find_all(&unsafe_matcher) {
                findings.push(QualityFinding {
                    rule: "rust-unsafe".to_string(),
                    severity: Severity::Info,
                    message: "unsafe block — ensure safety invariants documented".to_string(),
                    line: node.start_pos().line() + 1,
                    snippet: truncate_text(&node.text(), 80),
                    suggestion: Some("Add // SAFETY: comment explaining invariants".to_string()),
                });
            }

            // R3: panic!/todo!/unimplemented! in non-test code
            check_keyword_pattern(
                source,
                "panic!",
                "rust-panic",
                Severity::Warning,
                "panic!() in production code",
                "Use anyhow::bail! or return Err()",
                &mut findings,
            );
            check_keyword_pattern(
                source,
                "todo!()",
                "rust-todo",
                Severity::Warning,
                "todo!() marker — incomplete implementation",
                "Implement or remove before release",
                &mut findings,
            );
            check_keyword_pattern(
                source,
                "unimplemented!()",
                "rust-unimplemented",
                Severity::Warning,
                "unimplemented!() — missing implementation",
                "Implement the function body",
                &mut findings,
            );

            // R4: .clone() in function signatures (potentially unnecessary)
            check_keyword_pattern(
                source,
                ".clone()",
                "rust-clone",
                Severity::Info,
                ".clone() usage — check if a reference would suffice",
                "Consider using a reference instead",
                &mut findings,
            );
        }

        SupportLang::Python => {
            // P1: Bare except
            check_keyword_pattern(
                source,
                "except:",
                "py-bare-except",
                Severity::Warning,
                "Bare except: catches all exceptions including SystemExit",
                "Specify exception type: except Exception:",
                &mut findings,
            );

            // P2: print() in non-test files
            if !path.contains("test") {
                check_keyword_pattern(
                    source,
                    "print(",
                    "py-print",
                    Severity::Info,
                    "print() in production code",
                    "Use logging module instead",
                    &mut findings,
                );
            }

            // P3: mutable default argument
            for line in source.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("def ")
                    && (trimmed.contains("=[]")
                        || trimmed.contains("= []")
                        || trimmed.contains("={}")
                        || trimmed.contains("= {}"))
                {
                    let line_num = source[..source.find(line).unwrap_or(0)].lines().count() + 1;
                    findings.push(QualityFinding {
                        rule: "py-mutable-default".to_string(),
                        severity: Severity::Error,
                        message: "Mutable default argument — shared across calls".to_string(),
                        line: line_num,
                        snippet: truncate_text(trimmed, 80),
                        suggestion: Some(
                            "Use None as default and create inside function".to_string(),
                        ),
                    });
                }
            }

            // P4: Star import
            check_keyword_pattern(
                source,
                "from ",
                "py-star-import",
                Severity::Warning,
                "Star import — pollutes namespace",
                "Import specific names",
                &mut findings,
            );
            // Refine: only flag lines with "import *"
            findings.retain(|f| f.rule != "py-star-import" || f.snippet.contains("import *"));
        }

        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => {
            // T1: console.log in non-test code
            if !path.contains("test") && !path.contains("spec") {
                check_keyword_pattern(
                    source,
                    "console.log(",
                    "ts-console-log",
                    Severity::Info,
                    "console.log() in production code",
                    "Use a proper logger",
                    &mut findings,
                );
            }

            // T2: any type usage
            check_keyword_pattern(
                source,
                ": any",
                "ts-any-type",
                Severity::Warning,
                "'any' type defeats TypeScript's type safety",
                "Use a specific type or unknown",
                &mut findings,
            );

            // T3: Empty catch block
            let catch_matcher = KindMatcher::new("catch_clause", lang);
            for node in root.find_all(&catch_matcher) {
                let text = node.text();
                // Check if catch body is empty or just a comment
                if text.contains("{}") || (text.contains("{ }") && text.len() < 30) {
                    findings.push(QualityFinding {
                        rule: "ts-empty-catch".to_string(),
                        severity: Severity::Warning,
                        message: "Empty catch block swallows errors".to_string(),
                        line: node.start_pos().line() + 1,
                        snippet: truncate_text(&text, 80),
                        suggestion: Some("Log or rethrow the error".to_string()),
                    });
                }
            }
        }

        SupportLang::Go => {
            // G1: Ignoring error return
            check_keyword_pattern(
                source,
                "_ = ",
                "go-ignored-error",
                Severity::Warning,
                "Ignored return value (possibly error)",
                "Handle the error explicitly",
                &mut findings,
            );

            // G2: fmt.Print in non-test code
            if !path.contains("test") {
                check_keyword_pattern(
                    source,
                    "fmt.Print",
                    "go-fmt-print",
                    Severity::Info,
                    "fmt.Print() in production code",
                    "Use a structured logger",
                    &mut findings,
                );
            }
        }

        _ => {}
    }

    // Calculate quality score (100 = perfect, deductions per finding)
    let deductions: f32 = findings
        .iter()
        .map(|f| match f.severity {
            Severity::Error => 10.0,
            Severity::Warning => 5.0,
            Severity::Info => 1.0,
        })
        .sum();
    let score = (100.0 - deductions).clamp(0.0, 100.0);

    QualityReport {
        path: path.to_string(),
        language: lang_name(lang).to_string(),
        findings,
        score,
    }
}

/// Check for a simple keyword pattern in source, recording findings.
fn check_keyword_pattern(
    source: &str,
    pattern: &str,
    rule: &str,
    severity: Severity,
    message: &str,
    suggestion: &str,
    findings: &mut Vec<QualityFinding>,
) {
    for (line_idx, line) in source.lines().enumerate() {
        // Skip test code
        if line.contains("#[test]") || line.contains("#[cfg(test)]") {
            break;
        }
        if line.contains(pattern) {
            findings.push(QualityFinding {
                rule: rule.to_string(),
                severity,
                message: message.to_string(),
                line: line_idx + 1,
                snippet: truncate_text(line.trim(), 80),
                suggestion: Some(suggestion.to_string()),
            });
        }
    }
}

fn truncate_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        text.to_string()
    } else {
        format!(
            "{}...",
            &text[..text.floor_char_boundary(max.saturating_sub(3))]
        )
    }
}

fn lang_name(lang: SupportLang) -> &'static str {
    match lang {
        SupportLang::Rust => "Rust",
        SupportLang::Python => "Python",
        SupportLang::TypeScript => "TypeScript",
        SupportLang::Tsx => "TSX",
        SupportLang::JavaScript => "JavaScript",
        SupportLang::Go => "Go",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_unwrap_detection() {
        let source = r#"
fn main() {
    let value = some_result().unwrap();
    let other = another().expect("reason");
}
"#;
        let report = analyze_source(source, SupportLang::Rust, "src/main.rs");
        let unwrap_findings: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.rule == "rust-unwrap")
            .collect();
        assert!(
            !unwrap_findings.is_empty(),
            "should detect .unwrap(): {:?}",
            report.findings
        );
    }

    #[test]
    fn test_rust_panic_detection() {
        let source = r#"
fn handler() {
    panic!("unexpected state");
}
"#;
        let report = analyze_source(source, SupportLang::Rust, "src/handler.rs");
        assert!(
            report.findings.iter().any(|f| f.rule == "rust-panic"),
            "should detect panic!: {:?}",
            report.findings
        );
    }

    #[test]
    fn test_python_bare_except() {
        let source = r#"
try:
    do_something()
except:
    pass
"#;
        let report = analyze_source(source, SupportLang::Python, "src/handler.py");
        assert!(
            report.findings.iter().any(|f| f.rule == "py-bare-except"),
            "should detect bare except: {:?}",
            report.findings
        );
    }

    #[test]
    fn test_python_mutable_default() {
        let source = r#"
def process(items=[]):
    items.append(1)
    return items
"#;
        let report = analyze_source(source, SupportLang::Python, "src/process.py");
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.rule == "py-mutable-default"),
            "should detect mutable default: {:?}",
            report.findings
        );
    }

    #[test]
    fn test_typescript_any_type() {
        let source = r#"
function handler(data: any): void {
    console.log(data);
}
"#;
        let report = analyze_source(source, SupportLang::TypeScript, "src/handler.ts");
        assert!(
            report.findings.iter().any(|f| f.rule == "ts-any-type"),
            "should detect any type: {:?}",
            report.findings
        );
    }

    #[test]
    fn test_quality_score_calculation() {
        let source = "fn clean() -> Result<()> { Ok(()) }";
        let report = analyze_source(source, SupportLang::Rust, "src/clean.rs");
        assert_eq!(report.score, 100.0, "clean code should score 100");

        let bad_source = r#"
fn bad() {
    panic!("oops");
    let x = y.unwrap();
}
"#;
        let report = analyze_source(bad_source, SupportLang::Rust, "src/bad.rs");
        assert!(
            report.score < 100.0,
            "bad code should score < 100: {}",
            report.score
        );
    }

    #[test]
    fn test_format_text() {
        let report = QualityReport {
            path: "src/main.rs".to_string(),
            language: "Rust".to_string(),
            findings: vec![QualityFinding {
                rule: "rust-unwrap".to_string(),
                severity: Severity::Warning,
                message: ".unwrap() without context".to_string(),
                line: 5,
                snippet: "value.unwrap()".to_string(),
                suggestion: Some("Use expect()".to_string()),
            }],
            score: 95.0,
        };
        let text = report.format_text();
        assert!(text.contains("src/main.rs"));
        assert!(text.contains("95"));
        assert!(text.contains("rust-unwrap"));
    }
}
