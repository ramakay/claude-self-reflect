//! AST-anchored memory: pin facts to structural code nodes, not text locations.
//!
//! Anchors are content-addressed (lookup by function name within file) so
//! formatting and relocation inside a file never invalidate them. Only a
//! semantic change to the anchored node's normalized body fires `Modified`.
//!
//! v9.3 scope: whitespace-only normalization (no comment stripping), single-file
//! content addressing (cross-file moves read as `Broken` — accepted for v9.3).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use ast_grep_core::matcher::KindMatcher;
use ast_grep_core::tree_sitter::LanguageExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::ast_analysis::{extract_name_from_def, func_kinds, lang_from_path_str};

/// Maximum source size we will parse for anchoring (bytes).
const MAX_ANCHOR_FILE_BYTES: u64 = 512 * 1024;
/// Maximum anchors captured per file.
const MAX_ANCHORS_PER_FILE: usize = 50;

/// A fact anchor pinned to a structural code node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionAnchor {
    pub file: String,
    pub node_kind: String,
    pub name: String,
    /// First 16 hex chars of SHA-256 over the whitespace-normalized node text.
    pub body_hash: String,
}

/// Graded verification verdict — never a boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorVerdict {
    Intact,
    Modified,
    Broken,
}

/// Strip ALL whitespace so formatter churn (rustfmt/prettier) never drifts hashes.
pub fn normalize_code(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 16-hex-char truncated SHA-256 of normalized text.
pub fn hash_normalized(text: &str) -> String {
    let digest = Sha256::digest(normalize_code(text).as_bytes());
    hex_prefix(&digest, 16)
}

fn hex_prefix(bytes: &[u8], chars: usize) -> String {
    let mut s = String::with_capacity(chars);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
        if s.len() >= chars {
            break;
        }
    }
    s.truncate(chars);
    s
}

/// Prefix the bare name with its container (impl/class) name when present:
/// `new` inside `impl TokenCache` → `TokenCache::new`. Prevents UNIQUE collisions
/// for two same-named methods in different impl blocks of one file.
fn qualify_name<D: ast_grep_core::Doc>(
    node: &ast_grep_core::NodeMatch<'_, D>,
    bare: String,
) -> String {
    const CONTAINER_KINDS: &[&str] = &[
        "impl_item",         // Rust
        "class_definition",  // Python
        "class_declaration", // TS/JS
    ];
    for ancestor in node.ancestors() {
        let kind = ancestor.kind();
        if CONTAINER_KINDS.contains(&kind.as_ref()) {
            // tree-sitter field access: impl_item has `type`, classes have `name`
            let container = ancestor
                .field("name")
                .or_else(|| ancestor.field("type"))
                .map(|n| n.text().to_string());
            if let Some(c) = container {
                return format!("{}::{}", c, bare);
            }
        }
    }
    bare
}

/// Parse a source file and return anchors for every named function-like node.
/// Returns empty on unsupported language, oversized file, or parse panic.
pub fn capture_file_anchors(path: &Path) -> Vec<FunctionAnchor> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    if meta.len() > MAX_ANCHOR_FILE_BYTES {
        return Vec::new();
    }
    let Some(lang) = lang_from_path_str(&path.to_string_lossy()) else {
        // Unsupported language: emit one file-level sentinel so episodes can
        // still track whole-file drift (Swift, C#, etc.).
        let Ok(source) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        return vec![FunctionAnchor {
            file: path.to_string_lossy().to_string(),
            node_kind: "file".to_string(),
            name,
            body_hash: hash_normalized(&source),
        }];
    };
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let file = path.to_string_lossy().to_string();

    catch_unwind(AssertUnwindSafe(|| {
        let grep = lang.ast_grep(&source);
        let root = grep.root();
        let mut anchors = Vec::new();
        for kind in func_kinds(lang) {
            let matcher = KindMatcher::new(kind, lang);
            for node in root.find_all(&matcher) {
                if anchors.len() >= MAX_ANCHORS_PER_FILE {
                    break;
                }
                if let Some(name) = extract_name_from_def(&node, lang) {
                    anchors.push(FunctionAnchor {
                        file: file.clone(),
                        node_kind: (*kind).to_string(),
                        name: qualify_name(&node, name),
                        body_hash: hash_normalized(&node.text()),
                    });
                }
            }
        }
        anchors
    }))
    .unwrap_or_default()
}

/// Re-parse the anchor's file (resolved against `root` if relative) and grade it.
/// Content-addressed: looks the node up by name, so relocation within the file
/// and reformatting are `Intact`. Missing file or node → `Broken`.
pub fn verify_anchor(anchor: &FunctionAnchor, root: &Path) -> AnchorVerdict {
    let p = Path::new(&anchor.file);
    let path = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    let current = capture_file_anchors(&path);
    if current.is_empty() {
        return AnchorVerdict::Broken;
    }
    match current.iter().find(|a| a.name == anchor.name) {
        None => AnchorVerdict::Broken,
        Some(a) if a.body_hash == anchor.body_hash => AnchorVerdict::Intact,
        Some(_) => AnchorVerdict::Modified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const RUST_SRC: &str =
        "fn validate_token(t: &str) -> bool {\n    t.len() > 8\n}\n\nfn other() {}\n";

    #[test]
    fn normalize_strips_all_whitespace() {
        assert_eq!(
            normalize_code("fn  a( ) {\n\treturn 1;\n}"),
            normalize_code("fn a() { return 1; }")
        );
    }

    #[test]
    fn captures_function_anchors_from_rust_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.rs");
        fs::write(&path, RUST_SRC).unwrap();
        let anchors = capture_file_anchors(&path);
        let names: Vec<&str> = anchors.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"validate_token"));
        assert!(names.contains(&"other"));
        assert_eq!(anchors[0].node_kind, "function_item");
        assert_eq!(anchors[0].body_hash.len(), 16);
    }

    #[test]
    fn verify_intact_after_reformat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.rs");
        fs::write(&path, RUST_SRC).unwrap();
        let anchor = capture_file_anchors(&path)
            .into_iter()
            .find(|a| a.name == "validate_token")
            .unwrap();
        // Reformat: extra whitespace + moved below `other`
        fs::write(
            &path,
            "fn other() {}\n\nfn validate_token(t: &str)  ->  bool {\n        t.len() > 8\n}\n",
        )
        .unwrap();
        assert_eq!(verify_anchor(&anchor, dir.path()), AnchorVerdict::Intact);
    }

    #[test]
    fn verify_modified_on_semantic_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.rs");
        fs::write(&path, RUST_SRC).unwrap();
        let anchor = capture_file_anchors(&path)
            .into_iter()
            .find(|a| a.name == "validate_token")
            .unwrap();
        fs::write(
            &path,
            "fn validate_token(t: &str) -> bool {\n    t.len() > 12\n}\n",
        )
        .unwrap();
        assert_eq!(verify_anchor(&anchor, dir.path()), AnchorVerdict::Modified);
    }

    #[test]
    fn verify_broken_when_function_gone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.rs");
        fs::write(&path, RUST_SRC).unwrap();
        let anchor = capture_file_anchors(&path)
            .into_iter()
            .find(|a| a.name == "validate_token")
            .unwrap();
        fs::write(&path, "fn other() {}\n").unwrap();
        assert_eq!(verify_anchor(&anchor, dir.path()), AnchorVerdict::Broken);
        fs::remove_file(&path).unwrap();
        assert_eq!(verify_anchor(&anchor, dir.path()), AnchorVerdict::Broken);
    }

    #[test]
    fn unsupported_language_yields_file_level_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("RadioSheet.swift");
        std::fs::write(&path, "class RadioSheet { func show() {} }").unwrap();
        let anchors = capture_file_anchors(&path);
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].node_kind, "file");
        assert_eq!(anchors[0].name, "RadioSheet.swift");
        assert!(!anchors[0].body_hash.is_empty());
    }

    #[test]
    fn file_level_anchor_verifies_intact_then_modified() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Player.swift");
        std::fs::write(&path, "struct Player {}").unwrap();
        let anchor = capture_file_anchors(&path).remove(0);
        assert_eq!(verify_anchor(&anchor, dir.path()), AnchorVerdict::Intact);
        std::fs::write(&path, "struct Player { var ring: Bool }").unwrap();
        assert_eq!(verify_anchor(&anchor, dir.path()), AnchorVerdict::Modified);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(verify_anchor(&anchor, dir.path()), AnchorVerdict::Broken);
    }

    #[test]
    fn missing_file_still_yields_no_anchors() {
        let anchors = capture_file_anchors(std::path::Path::new("/nonexistent/thing.swift"));
        assert!(anchors.is_empty());
    }

    #[test]
    fn impl_methods_get_qualified_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.rs");
        std::fs::write(&path, "struct A; struct B;\nimpl A { fn new() -> Self { A } }\nimpl B { fn new() -> Self { B } }\n").unwrap();
        let anchors = capture_file_anchors(&path);
        let names: Vec<&str> = anchors.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"A::new"));
        assert!(names.contains(&"B::new"));
    }
}
