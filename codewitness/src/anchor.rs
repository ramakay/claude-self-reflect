use std::path::{Path, PathBuf};

/// A location inside a repository that a claim is anchored to.
///
/// `path` is repository-relative, using the OS's own path separators for
/// convenience of construction; it is converted to git's forward-slash
/// convention internally before touching any git object.
///
/// `symbol` is an informational label (e.g. a function or type name).
/// `codewitness` does not resolve symbols to byte ranges itself — that is
/// AST-layer work outside this crate's scope. Callers who want
/// symbol-precise witnessing should resolve `symbol` to a `span` themselves
/// (e.g. via `ast-grep` or similar) before constructing the `Anchor`.
///
/// `span` is a 1-based, inclusive `(start_line, end_line)` range. `None`
/// means "the whole file".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Anchor {
    pub path: PathBuf,
    pub symbol: Option<String>,
    pub span: Option<(u32, u32)>,
}

impl Anchor {
    /// A whole-file anchor with no symbol or span.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            symbol: None,
            span: None,
        }
    }

    /// Attach an informational symbol name. Does not affect what bytes are
    /// stamped — see the type-level docs.
    pub fn with_symbol(mut self, symbol: impl Into<String>) -> Self {
        self.symbol = Some(symbol.into());
        self
    }

    /// Restrict stamping to a 1-based, inclusive line range.
    pub fn with_span(mut self, start_line: u32, end_line: u32) -> Self {
        self.span = Some((start_line, end_line));
        self
    }
}

/// Render any repo-relative `path` (an [`Anchor::path`] or a bare path from
/// a tree walk) with forward slashes, as git stores paths internally —
/// regardless of the OS's own separator. Free function, shared by every
/// committed-tree blob lookup in [`crate::Auditor`] (symbol-anchored or
/// whole-file), so there is exactly one normalization rule instead of one
/// per call site.
pub(crate) fn to_git_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
