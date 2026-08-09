use crate::import;

/// Encode a path the way Claude Code names its `~/.claude/projects` folders.
///
/// Claude Code does `path.replace(/[^a-zA-Z0-9]/g, "-")` (verified in the shipped
/// binary, 2.1.226). That regex carries no `u` flag, so it runs over UTF-16 code
/// units, not scalar values: an accented BMP character costs one dash, but a
/// non-BMP one (emoji, rarer CJK) is a surrogate pair and costs *two*. Mapping per
/// `char` would emit one dash there and miss the stored folder — the same silent
/// empty-result failure this whole branch exists to fix, just narrower.
fn encode_project_folder(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                out.push('-');
            }
        }
    }
    out
}

/// Resolve project name from a CWD path string.
/// Pure function — testable without environment variable manipulation.
pub fn resolve_project_from_cwd(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }

    // Windows drive path ("D:\Claude", or "\\?\D:\Claude" after canonicalize):
    // stored project names are Claude Code's ~/.claude/projects folder names,
    // which encode the FULL path with every non-alphanumeric char as '-'
    // ("D:\Claude" → "D--Claude"). The last-component fallback below returned
    // "Claude", which matches no stored project and silently emptied every
    // project-scoped search. Hooks receive the project root as cwd, so no
    // component-walking is needed here.
    let stripped = cwd.strip_prefix(r"\\?\").unwrap_or(cwd);
    let trimmed = stripped.trim_end_matches(['\\', '/']);
    if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
        return Some(encode_project_folder(trimmed));
    }

    let path = std::path::Path::new(cwd);
    let dir_name = path.file_name()?.to_string_lossy().to_string();

    // If it looks like a Claude projects directory (dash-separated), normalize it
    if dir_name.starts_with('-') && dir_name.contains("projects") {
        return Some(import::normalize_project_name(&dir_name));
    }

    // Walk up path components to find one after "projects"
    // This handles subdirectories like /Users/name/projects/my-app/src/engine
    let components: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
    for (i, comp) in components.iter().enumerate() {
        if *comp == "projects" && i + 1 < components.len() {
            return Some(components[i + 1].to_string());
        }
    }

    // Fallback: last path component
    Some(dir_name)
}

/// Resolve the current project from the `MCP_CLIENT_CWD` environment variable.
///
/// Claude Code sets `MCP_CLIENT_CWD` to the user's working directory when invoking
/// MCP tools. We extract the project name using `resolve_project_from_cwd`.
///
/// Returns `None` if the env var is not set.
pub fn resolve_current_project() -> Option<String> {
    let cwd = std::env::var("MCP_CLIENT_CWD").ok()?;
    resolve_project_from_cwd(&cwd)
}

/// Normalize a project scope parameter.
///
/// - `None` → auto-detect from `MCP_CLIENT_CWD`
/// - `Some("all")` (any case) → `None` (search all projects)
/// - `Some(name)` → `Some(name)` (specific project)
///
/// Returns `(effective_project, scope_label)` where scope_label is for display.
pub fn normalize_project_scope(project: Option<&str>) -> (Option<String>, String) {
    match project {
        Some(p) if p.eq_ignore_ascii_case("all") => (None, "all".to_string()),
        Some(p) if !p.is_empty() => (Some(p.to_string()), p.to_string()),
        _ => {
            // Auto-detect from environment
            match resolve_current_project() {
                Some(p) => {
                    let label = p.clone();
                    (Some(p), label)
                }
                None => (None, "all".to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_scope_all() {
        let (project, label) = normalize_project_scope(Some("all"));
        assert!(project.is_none());
        assert_eq!(label, "all");
    }

    #[test]
    fn test_normalize_scope_all_case_insensitive() {
        let (project, _) = normalize_project_scope(Some("ALL"));
        assert!(project.is_none());
        let (project, _) = normalize_project_scope(Some("All"));
        assert!(project.is_none());
    }

    #[test]
    fn test_normalize_scope_specific() {
        let (project, label) = normalize_project_scope(Some("my-project"));
        assert_eq!(project, Some("my-project".to_string()));
        assert_eq!(label, "my-project");
    }

    #[test]
    fn test_normalize_scope_none_no_env() {
        // When MCP_CLIENT_CWD is not set, falls back to "all"
        let (_project, label) = normalize_project_scope(None);
        assert!(!label.is_empty());
    }

    #[test]
    fn test_normalize_scope_empty() {
        let (project, label) = normalize_project_scope(Some(""));
        assert_eq!(
            label,
            if project.is_some() {
                project.as_deref().unwrap()
            } else {
                "all"
            }
        );
    }

    // Pure function tests — no env var manipulation needed
    #[test]
    fn test_resolve_from_cwd_simple_project() {
        let result = resolve_project_from_cwd("/Users/name/projects/claude-self-reflect");
        assert_eq!(result, Some("claude-self-reflect".to_string()));
    }

    #[test]
    fn test_resolve_from_cwd_subdirectory() {
        let result =
            resolve_project_from_cwd("/Users/name/projects/claude-self-reflect/src/engine");
        assert_eq!(result, Some("claude-self-reflect".to_string()));
    }

    #[test]
    fn test_resolve_from_cwd_claude_dir_format() {
        let result = resolve_project_from_cwd(
            "/Users/name/.claude/projects/-Users-name-projects-claude-self-reflect",
        );
        assert_eq!(result, Some("claude-self-reflect".to_string()));
    }

    #[test]
    fn test_resolve_from_cwd_empty() {
        assert_eq!(resolve_project_from_cwd(""), None);
    }

    #[test]
    fn test_resolve_from_cwd_no_projects_segment() {
        let result = resolve_project_from_cwd("/tmp/something/mydir");
        assert_eq!(result, Some("mydir".to_string()));
    }

    /// Pins the stored project-name contract for Windows drive paths.
    ///
    /// Deliberately NOT `#[cfg(windows)]`-gated: the code path under test is pure
    /// string handling with no platform-conditional code, so gating it would only
    /// hide the regression from the CI that actually runs — which has no Windows
    /// job. A hook can deliver `cwd` raw, `\\?\`-canonicalized, with a trailing
    /// separator, or any combination of those, and every shape has to land on the
    /// same stored name or project-scoped search silently returns nothing.
    #[test]
    fn test_resolve_from_cwd_windows_drive_paths() {
        for cwd in [
            r"D:\Claude",
            r"\\?\D:\Claude",
            "D:\\Claude\\",
            "\\\\?\\D:\\Claude\\",
        ] {
            assert_eq!(
                resolve_project_from_cwd(cwd),
                Some("D--Claude".to_string()),
                "cwd {cwd:?} must encode to the stored project name"
            );
        }

        // The encoding covers the whole path, not just the last component — that
        // was the original bug: "D:\Claude" resolved to "Claude", which matches no
        // stored project.
        assert_eq!(
            resolve_project_from_cwd(r"D:\Proyectos\claude-self-reflect"),
            Some("D--Proyectos-claude-self-reflect".to_string())
        );
    }

    /// Claude Code's encoder is `replace(/[^a-zA-Z0-9]/g, "-")` with no `u` flag,
    /// so it counts UTF-16 code units: one dash for a BMP character, two for a
    /// surrogate pair. Non-ASCII project directories are ordinary outside English
    /// locales, so getting this wrong reproduces the original miss on exactly the
    /// paths least likely to be reported.
    #[test]
    fn test_resolve_from_cwd_non_ascii_drive_paths() {
        // Every non-ASCII character here is BMP: one code unit, one dash.
        assert_eq!(
            resolve_project_from_cwd(r"D:\Proyectos\Español"),
            Some("D--Proyectos-Espa-ol".to_string())
        );
        // ':' + '\' + 7 Cyrillic + '\' + 3 Cyrillic = 13 code units after "C".
        assert_eq!(
            resolve_project_from_cwd(r"C:\Проекты\бот"),
            Some(format!("C{}", "-".repeat(13)))
        );

        // A non-BMP character is a surrogate pair: two code units, two dashes.
        assert_eq!(
            resolve_project_from_cwd("D:\\🚀x"),
            Some("D----x".to_string())
        );
    }
}
