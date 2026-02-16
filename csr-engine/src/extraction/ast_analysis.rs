//! AST-based code analysis for code-aware conversation enrichment.
//!
//! Uses ast-grep-core to parse code blocks found in conversations and extract
//! structural metadata: function names, struct/class definitions, imports, and patterns.
//! This enables queries like "find conversations where I modified dispatch_hook".

use std::collections::BTreeSet;

use ast_grep_core::matcher::KindMatcher;
use ast_grep_core::tree_sitter::LanguageExt;
use ast_grep_language::SupportLang;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Code context extracted from a conversation's code blocks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeContext {
    /// Function/method names found or modified
    pub functions: BTreeSet<String>,
    /// Struct/class/interface names found or modified
    pub types: BTreeSet<String>,
    /// Import paths
    pub imports: BTreeSet<String>,
    /// Programming languages detected
    pub languages: BTreeSet<String>,
    /// Code patterns detected (e.g., "async_fn", "error_handling", "unsafe")
    pub patterns: BTreeSet<String>,
}

impl CodeContext {
    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
            && self.types.is_empty()
            && self.imports.is_empty()
            && self.languages.is_empty()
            && self.patterns.is_empty()
    }

    /// Format as searchable text for embedding.
    pub fn to_search_text(&self) -> String {
        let mut parts = Vec::new();

        if !self.functions.is_empty() {
            let fns: Vec<&str> = self.functions.iter().map(|s| s.as_str()).collect();
            parts.push(format!("FUNCTIONS: {}", fns.join(", ")));
        }
        if !self.types.is_empty() {
            let ts: Vec<&str> = self.types.iter().map(|s| s.as_str()).collect();
            parts.push(format!("TYPES: {}", ts.join(", ")));
        }
        if !self.imports.is_empty() {
            let imps: Vec<&str> = self.imports.iter().map(|s| s.as_str()).collect();
            parts.push(format!("IMPORTS: {}", imps.join(", ")));
        }
        if !self.patterns.is_empty() {
            let pats: Vec<&str> = self.patterns.iter().map(|s| s.as_str()).collect();
            parts.push(format!("PATTERNS: {}", pats.join(", ")));
        }
        if !self.languages.is_empty() {
            let langs: Vec<&str> = self.languages.iter().map(|s| s.as_str()).collect();
            parts.push(format!("LANGUAGES: {}", langs.join(", ")));
        }

        parts.join("\n")
    }

    fn merge(&mut self, other: &CodeContext) {
        self.functions.extend(other.functions.iter().cloned());
        self.types.extend(other.types.iter().cloned());
        self.imports.extend(other.imports.iter().cloned());
        self.languages.extend(other.languages.iter().cloned());
        self.patterns.extend(other.patterns.iter().cloned());
    }
}

/// A code block extracted from a conversation message.
struct CodeBlock {
    source: String,
    lang: Option<SupportLang>,
}

/// Extract code context from all messages in a conversation.
/// Scans for:
/// 1. Fenced markdown code blocks (```rust ... ```)
/// 2. Edit/Write tool_use blocks (old_string, new_string)
/// 3. tool_result content that contains code
pub fn extract_code_context(messages: &[Value]) -> CodeContext {
    let mut ctx = CodeContext::default();

    for msg in messages {
        let msg_data = super::get_message_data(msg);

        // Check for tool_use blocks (Edit/Write with code)
        if let Some(content) = msg_data.get("content").and_then(|v| v.as_array()) {
            for item in content {
                if item.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                    extract_from_tool_use(item, &mut ctx);
                }
                if item.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                    if let Some(text) = item.get("content").and_then(|v| v.as_str()) {
                        extract_from_fenced_blocks(text, &mut ctx);
                    }
                }
            }
        }

        // Check for plain text content with fenced blocks
        if let Some(text) = msg_data.get("content").and_then(|v| v.as_str()) {
            extract_from_fenced_blocks(text, &mut ctx);
        }
    }

    ctx
}

/// Extract code context from a tool_use block (Edit, Write, MultiEdit).
fn extract_from_tool_use(item: &Value, ctx: &mut CodeContext) {
    let tool_name = item
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();

    let input = match item.get("input") {
        Some(v) => v,
        None => return,
    };

    // Detect language from file_path
    let file_path = input.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
    let lang = lang_from_path(file_path);

    if tool_name.contains("edit") || tool_name.contains("write") || tool_name.contains("multiedit")
    {
        // Extract from old_string and new_string
        for field in &["old_string", "new_string", "content"] {
            if let Some(code) = input.get(*field).and_then(|v| v.as_str()) {
                if let Some(lang) = lang {
                    let block_ctx = analyze_code(code, lang);
                    ctx.merge(&block_ctx);
                }
            }
        }
    }
}

/// Extract code from fenced markdown blocks (```lang ... ```).
fn extract_from_fenced_blocks(text: &str, ctx: &mut CodeContext) {
    let blocks = parse_fenced_blocks(text);
    for block in blocks {
        if let Some(lang) = block.lang {
            let block_ctx = analyze_code(&block.source, lang);
            ctx.merge(&block_ctx);
        }
    }
}

/// Parse fenced code blocks from markdown text.
fn parse_fenced_blocks(text: &str) -> Vec<CodeBlock> {
    let mut blocks = Vec::new();
    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") && trimmed.len() > 3 {
            let lang_tag = trimmed[3..].trim().split_whitespace().next().unwrap_or("");
            let (lang, _) = lang_from_tag(lang_tag);

            let mut source = String::new();
            for inner_line in lines.by_ref() {
                if inner_line.trim().starts_with("```") {
                    break;
                }
                source.push_str(inner_line);
                source.push('\n');
            }

            if !source.trim().is_empty() {
                blocks.push(CodeBlock { source, lang });
            }
        }
    }

    blocks
}

/// Analyze a code snippet with ast-grep and extract structural metadata.
fn analyze_code(source: &str, lang: SupportLang) -> CodeContext {
    // Skip very short snippets (likely not useful)
    if source.len() < 10 {
        return CodeContext::default();
    }

    // Use catch_unwind for robustness (tree-sitter can panic on malformed input)
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        analyze_code_inner(source, lang)
    }));

    result.unwrap_or_default()
}

fn analyze_code_inner(source: &str, lang: SupportLang) -> CodeContext {
    let mut ctx = CodeContext::default();
    ctx.languages.insert(lang_display_name(lang).to_string());

    let grep = lang.ast_grep(source);
    let root = grep.root();

    // Extract function/method definitions
    for kind in func_kinds(lang) {
        let matcher = KindMatcher::new(kind, lang);
        for node in root.find_all(&matcher) {
            if let Some(name) = extract_name_from_def(&node, lang) {
                ctx.functions.insert(name);
            }
        }
    }

    // Extract type definitions (struct, class, interface, enum, trait)
    for kind in type_kinds(lang) {
        let matcher = KindMatcher::new(kind, lang);
        for node in root.find_all(&matcher) {
            if let Some(name) = extract_name_from_def(&node, lang) {
                ctx.types.insert(name);
            }
        }
    }

    // Extract imports
    for kind in import_kinds(lang) {
        let matcher = KindMatcher::new(kind, lang);
        for node in root.find_all(&matcher) {
            let text = node.text().to_string();
            // Truncate long import lines
            let import = if text.len() > 100 {
                format!("{}...", &text[..97])
            } else {
                text
            };
            ctx.imports.insert(import);
        }
    }

    // Detect code patterns
    detect_patterns(source, lang, &mut ctx);

    ctx
}

/// Get function definition kind names for a language.
fn func_kinds(lang: SupportLang) -> &'static [&'static str] {
    match lang {
        SupportLang::Rust => &["function_item"],
        SupportLang::Python => &["function_definition"],
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => {
            &["function_declaration", "method_definition", "arrow_function"]
        }
        SupportLang::Go => &["function_declaration", "method_declaration"],
        _ => &["function_definition"],
    }
}

/// Get type definition kind names for a language.
fn type_kinds(lang: SupportLang) -> &'static [&'static str] {
    match lang {
        SupportLang::Rust => &["struct_item", "enum_item", "trait_item", "type_item"],
        SupportLang::Python => &["class_definition"],
        SupportLang::TypeScript | SupportLang::Tsx => {
            &["class_declaration", "interface_declaration", "type_alias_declaration", "enum_declaration"]
        }
        SupportLang::JavaScript => &["class_declaration"],
        SupportLang::Go => &["type_declaration"],
        _ => &["class_definition"],
    }
}

/// Get import kind names for a language.
fn import_kinds(lang: SupportLang) -> &'static [&'static str] {
    match lang {
        SupportLang::Rust => &["use_declaration"],
        SupportLang::Python => &["import_statement", "import_from_statement"],
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => {
            &["import_statement"]
        }
        SupportLang::Go => &["import_declaration"],
        _ => &["import_statement"],
    }
}

/// Extract the name identifier from a definition node.
fn extract_name_from_def<D: ast_grep_core::Doc>(
    node: &ast_grep_core::NodeMatch<'_, D>,
    _lang: SupportLang,
) -> Option<String> {
    // The name is typically the first named child with kind "identifier" or "name"
    for child in node.children() {
        let kind = child.kind();
        let kind_str = kind.as_ref();
        if kind_str == "identifier"
            || kind_str == "name"
            || kind_str == "type_identifier"
            || kind_str == "property_identifier"
        {
            return Some(child.text().to_string());
        }
    }
    None
}

/// Detect high-level code patterns via keyword matching.
fn detect_patterns(source: &str, lang: SupportLang, ctx: &mut CodeContext) {
    // Simple keyword-based pattern detection (fast, doesn't need AST)
    let lower = source.to_lowercase();

    match lang {
        SupportLang::Rust => {
            if lower.contains("async fn") || lower.contains("async move") {
                ctx.patterns.insert("async".to_string());
            }
            if lower.contains("unsafe") {
                ctx.patterns.insert("unsafe".to_string());
            }
            if lower.contains(".unwrap()") {
                ctx.patterns.insert("unwrap_usage".to_string());
            }
            if lower.contains("impl ") && lower.contains(" for ") {
                ctx.patterns.insert("trait_impl".to_string());
            }
            if lower.contains("#[test]") || lower.contains("#[tokio::test]") {
                ctx.patterns.insert("test".to_string());
            }
            if lower.contains("result<") || lower.contains("-> result") {
                ctx.patterns.insert("error_handling".to_string());
            }
        }
        SupportLang::Python => {
            if lower.contains("async def") || lower.contains("await ") {
                ctx.patterns.insert("async".to_string());
            }
            if lower.contains("try:") && lower.contains("except") {
                ctx.patterns.insert("error_handling".to_string());
            }
            if lower.contains("def test_") || lower.contains("@pytest") {
                ctx.patterns.insert("test".to_string());
            }
            if lower.contains("class ") && lower.contains("(") {
                ctx.patterns.insert("inheritance".to_string());
            }
        }
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => {
            if lower.contains("async ") || lower.contains("await ") {
                ctx.patterns.insert("async".to_string());
            }
            if lower.contains("try {") || lower.contains("try{") {
                ctx.patterns.insert("error_handling".to_string());
            }
            if lower.contains("describe(") || lower.contains("it(") || lower.contains("test(") {
                ctx.patterns.insert("test".to_string());
            }
            if lower.contains("interface ") || lower.contains(": type") {
                ctx.patterns.insert("typing".to_string());
            }
        }
        SupportLang::Go => {
            if lower.contains("go func") || lower.contains("goroutine") {
                ctx.patterns.insert("concurrency".to_string());
            }
            if lower.contains("if err != nil") {
                ctx.patterns.insert("error_handling".to_string());
            }
            if lower.contains("func test") {
                ctx.patterns.insert("test".to_string());
            }
        }
        _ => {}
    }
}

/// Map a file path string to a SupportLang (public for quality module).
pub fn lang_from_path_str(path: &str) -> Option<SupportLang> {
    lang_from_path(path)
}

/// Map a file extension to a SupportLang.
fn lang_from_path(path: &str) -> Option<SupportLang> {
    let ext = path.rsplit('.').next()?;
    match ext.to_lowercase().as_str() {
        "rs" => Some(SupportLang::Rust),
        "py" => Some(SupportLang::Python),
        "ts" => Some(SupportLang::TypeScript),
        "tsx" => Some(SupportLang::Tsx),
        "js" | "mjs" | "jsx" => Some(SupportLang::JavaScript),
        "go" => Some(SupportLang::Go),
        _ => None,
    }
}

/// Map a markdown language tag to a SupportLang.
fn lang_from_tag(tag: &str) -> (Option<SupportLang>, &str) {
    match tag.to_lowercase().as_str() {
        "rust" | "rs" => (Some(SupportLang::Rust), "Rust"),
        "python" | "py" => (Some(SupportLang::Python), "Python"),
        "typescript" | "ts" => (Some(SupportLang::TypeScript), "TypeScript"),
        "tsx" => (Some(SupportLang::Tsx), "TSX"),
        "javascript" | "js" => (Some(SupportLang::JavaScript), "JavaScript"),
        "go" | "golang" => (Some(SupportLang::Go), "Go"),
        _ => (None, tag),
    }
}

/// Human-readable language name.
fn lang_display_name(lang: SupportLang) -> &'static str {
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
    use serde_json::json;

    #[test]
    fn test_analyze_rust_code() {
        let source = r#"
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct Engine {
    storage: Arc<Storage>,
}

impl Engine {
    pub async fn new(db_path: &Path) -> Result<Self> {
        let storage = Arc::new(Storage::open(db_path)?);
        Ok(Self { storage })
    }

    pub fn flush_index(&self) {
        // flush
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_engine() {}
}
"#;

        let ctx = analyze_code(source, SupportLang::Rust);
        assert!(ctx.functions.contains("new"), "should find 'new' function: {:?}", ctx.functions);
        assert!(ctx.functions.contains("flush_index"), "should find 'flush_index': {:?}", ctx.functions);
        assert!(ctx.types.contains("Engine"), "should find 'Engine' struct: {:?}", ctx.types);
        assert!(ctx.languages.contains("Rust"));
        assert!(ctx.patterns.contains("async"), "should detect async pattern: {:?}", ctx.patterns);
        assert!(ctx.patterns.contains("error_handling"), "should detect error handling: {:?}", ctx.patterns);
        assert!(ctx.patterns.contains("test"), "should detect test pattern: {:?}", ctx.patterns);
    }

    #[test]
    fn test_analyze_python_code() {
        let source = r#"
import os
from pathlib import Path

class MyHandler:
    def __init__(self, config):
        self.config = config

    async def handle_request(self, request):
        try:
            result = await self.process(request)
            return result
        except Exception as e:
            raise
"#;

        let ctx = analyze_code(source, SupportLang::Python);
        assert!(ctx.functions.contains("__init__"), "should find __init__: {:?}", ctx.functions);
        assert!(ctx.functions.contains("handle_request"), "should find handle_request: {:?}", ctx.functions);
        assert!(ctx.types.contains("MyHandler"), "should find MyHandler: {:?}", ctx.types);
        assert!(ctx.patterns.contains("async"));
        assert!(ctx.patterns.contains("error_handling"));
    }

    #[test]
    fn test_analyze_typescript_code() {
        let source = r#"
import { Router } from 'express';

interface UserConfig {
    name: string;
    email: string;
}

async function fetchUser(id: string): Promise<UserConfig> {
    try {
        const response = await fetch(`/api/users/${id}`);
        return response.json();
    } catch (error) {
        throw error;
    }
}

describe('fetchUser', () => {
    it('should fetch user', async () => {
        const user = await fetchUser('123');
        expect(user.name).toBeDefined();
    });
});
"#;

        let ctx = analyze_code(source, SupportLang::TypeScript);
        assert!(ctx.functions.contains("fetchUser"), "should find fetchUser: {:?}", ctx.functions);
        assert!(ctx.types.contains("UserConfig"), "should find UserConfig: {:?}", ctx.types);
        assert!(ctx.patterns.contains("async"));
        assert!(ctx.patterns.contains("error_handling"));
        assert!(ctx.patterns.contains("test"));
    }

    #[test]
    fn test_extract_from_fenced_blocks() {
        let text = r#"Here's the fix:

```rust
fn dispatch_hook(name: &str) -> Result<()> {
    match name {
        "start" => handle_start(),
        _ => Ok(()),
    }
}
```

And the Python version:

```python
def dispatch_hook(name: str) -> None:
    if name == "start":
        handle_start()
```
"#;

        let mut ctx = CodeContext::default();
        extract_from_fenced_blocks(text, &mut ctx);
        assert!(ctx.functions.contains("dispatch_hook"), "should find dispatch_hook: {:?}", ctx.functions);
        assert!(ctx.languages.contains("Rust"));
        assert!(ctx.languages.contains("Python"));
    }

    #[test]
    fn test_extract_from_tool_use() {
        let item = json!({
            "type": "tool_use",
            "name": "Edit",
            "input": {
                "file_path": "src/engine.rs",
                "old_string": "fn old_method(&self) {}",
                "new_string": "pub async fn new_method(&self) -> Result<()> {\n    self.storage.flush()?;\n    Ok(())\n}"
            }
        });

        let mut ctx = CodeContext::default();
        extract_from_tool_use(&item, &mut ctx);
        assert!(ctx.functions.contains("new_method") || ctx.functions.contains("old_method"),
            "should find function names: {:?}", ctx.functions);
        assert!(ctx.languages.contains("Rust"));
    }

    #[test]
    fn test_extract_code_context_full() {
        let messages = vec![
            json!({"role": "user", "content": "Fix the dispatch_hook function"}),
            json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "name": "Edit",
                    "input": {
                        "file_path": "src/hooks/mod.rs",
                        "old_string": "fn dispatch_hook() {}",
                        "new_string": "pub async fn dispatch_hook(name: &str, engine: &Engine) -> Result<()> {\n    Ok(())\n}"
                    }
                }]
            }),
        ];

        let ctx = extract_code_context(&messages);
        assert!(ctx.functions.contains("dispatch_hook"), "should find dispatch_hook: {:?}", ctx.functions);
        assert!(ctx.languages.contains("Rust"));
    }

    #[test]
    fn test_to_search_text() {
        let mut ctx = CodeContext::default();
        ctx.functions.insert("dispatch_hook".to_string());
        ctx.functions.insert("flush_index".to_string());
        ctx.types.insert("Engine".to_string());
        ctx.languages.insert("Rust".to_string());
        ctx.patterns.insert("async".to_string());

        let text = ctx.to_search_text();
        assert!(text.contains("FUNCTIONS: dispatch_hook, flush_index"));
        assert!(text.contains("TYPES: Engine"));
        assert!(text.contains("PATTERNS: async"));
        assert!(text.contains("LANGUAGES: Rust"));
    }

    #[test]
    fn test_lang_from_path() {
        assert_eq!(lang_from_path("src/engine.rs"), Some(SupportLang::Rust));
        assert_eq!(lang_from_path("scripts/import.py"), Some(SupportLang::Python));
        assert_eq!(lang_from_path("src/components/App.tsx"), Some(SupportLang::Tsx));
        assert_eq!(lang_from_path("README.md"), None);
    }

    #[test]
    fn test_empty_and_short_code() {
        let ctx = analyze_code("", SupportLang::Rust);
        assert!(ctx.is_empty());

        // Under 10 chars is skipped
        let ctx = analyze_code("x = 1", SupportLang::Python);
        assert!(ctx.is_empty());

        // Over 10 chars is parsed
        let ctx = analyze_code("result = some_function(arg1, arg2)", SupportLang::Python);
        assert!(ctx.languages.contains("Python"));
    }
}
