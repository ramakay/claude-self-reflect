//! V3-to-Story synthesis — generate 2-3 sentence stories from existing extraction data.
//!
//! Tier 1: V3 search_index → extract User Request + Solution Pattern → story (free, ~2ms)
//! Tier 2: Heuristic enrichment → template story (free, ~1ms)
//! Tier 3: Haiku escalation (only when Tier 1+2 insufficient, $0.004)

/// Synthesize a story from V3 extraction content.
/// Extracts `## User Request` and `## Solution Pattern` sections.
/// Returns None if content is empty or unparseable.
pub fn synthesize_story_from_v3(v3_content: &str, project: &str) -> Option<String> {
    if v3_content.trim().is_empty() {
        return None;
    }

    let user_request = extract_section(v3_content, "## User Request");
    let solution = extract_section(v3_content, "## Solution Pattern");
    let code_ctx = extract_section(v3_content, "## Code Context");

    // Build story from available sections
    let mut parts = Vec::new();

    if let Some(req) = user_request {
        let cleaned = clean_request_text(&req);
        if !cleaned.is_empty() {
            parts.push(cleaned);
        }
    }

    if let Some(sol) = solution {
        let files = extract_files_from_solution(&sol);
        if !files.is_empty() {
            let file_list: String = files.into_iter().take(3).collect::<Vec<_>>().join(", ");
            parts.push(format!("Modified {}", file_list));
        }
    }

    if let Some(ctx) = code_ctx {
        if let Some(lang) = ctx.lines().find(|l| l.starts_with("LANGUAGES:")) {
            parts.push(lang.trim().to_string());
        }
    }

    if parts.is_empty() {
        // Fallback: try to extract anything meaningful from the raw content
        let first_line = v3_content
            .lines()
            .find(|l| !l.trim().is_empty() && !l.starts_with('#'));
        if let Some(line) = first_line {
            let cleaned: String = line.trim().chars().take(200).collect();
            if cleaned.len() >= 10 {
                parts.push(format!("Session in {} project: {}", project, cleaned));
            }
        }
    }

    if parts.is_empty() {
        return None;
    }

    // Join and cap at 500 chars
    let story: String = parts.join(". ");
    let capped: String = story.chars().take(500).collect();
    Some(capped)
}

/// Synthesize a story from heuristic enrichment data.
pub fn synthesize_story_from_heuristic(heuristic: &str, project: &str) -> String {
    let tools = extract_heuristic_field(heuristic, "Tools:");
    let msgs = extract_heuristic_field(heuristic, "Messages:");
    let has_errors = heuristic.contains("Had errors: yes");

    let mut story = format!("Session in {} project", project);
    if let Some(tools_str) = tools {
        story.push_str(&format!(" using {}", tools_str));
    }
    if let Some(msgs_str) = msgs {
        story.push_str(&format!(" ({} messages)", msgs_str));
    }
    if has_errors {
        story.push_str(" with error investigation");
    }
    story.push('.');
    story
}

/// Determine if a conversation needs LLM narrative (Haiku) vs template.
pub fn needs_haiku(v3_content: Option<&str>, heuristic: Option<&str>, msg_count: usize) -> bool {
    match (v3_content, heuristic) {
        (None, None) => msg_count >= 5,
        (Some(v3), _) => {
            let req = extract_section(v3, "## User Request");
            req.map(|r| r.len() < 30).unwrap_or(true) && msg_count >= 30
        }
        (None, Some(_)) => msg_count >= 50,
    }
}

// --- Helpers ---

fn extract_section(content: &str, header: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.iter().position(|l| l.starts_with(header))?;
    let mut result = String::new();
    for line in &lines[start + 1..] {
        if line.starts_with("## ") {
            break;
        }
        if !line.trim().is_empty() {
            if !result.is_empty() {
                result.push(' ');
            }
            result.push_str(line.trim());
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn clean_request_text(raw: &str) -> String {
    raw.trim()
        .trim_matches('"')
        .replace("\\n", " ")
        .replace("\\\"", "\"")
        .chars()
        .take(200)
        .collect::<String>()
        .trim()
        .to_string()
}

fn extract_files_from_solution(solution: &str) -> Vec<String> {
    solution
        .lines()
        .filter(|l| l.starts_with("creation:") || l.starts_with("modification:"))
        .filter_map(|l| l.split(':').nth(1).map(|f| f.trim().to_string()))
        .collect()
}

fn extract_heuristic_field<'a>(heuristic: &'a str, field: &str) -> Option<&'a str> {
    heuristic.find(field).map(|pos| {
        let start = pos + field.len();
        let rest = &heuristic[start..];
        rest.split('\n').next().unwrap_or("").trim()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_synthesize_from_v3_with_user_request() {
        let v3_content = "## User Request\n\"Fix the authentication timeout bug in login flow\"\n\n## Solution Pattern\ncreation: auth.rs\n  Fixed timeout handling\n\n## Code Context\nLANGUAGES: Rust\n";
        let story = synthesize_story_from_v3(v3_content, "my-project");
        assert!(story.is_some());
        let s = story.unwrap();
        assert!(
            s.contains("authentication") || s.contains("timeout") || s.contains("login"),
            "story should mention the request: {}",
            s
        );
        assert!(s.len() <= 500);
        assert!(s.len() >= 20);
    }

    #[test]
    fn test_synthesize_from_v3_empty() {
        let story = synthesize_story_from_v3("", "proj");
        assert!(story.is_none());
    }

    #[test]
    fn test_synthesize_from_v3_with_solution_files() {
        let v3_content = "## User Request\n\"Add logging\"\n\n## Solution Pattern\nmodification: src/main.rs\ncreation: src/logger.rs\n";
        let story = synthesize_story_from_v3(v3_content, "test").unwrap();
        assert!(story.contains("src/main.rs") || story.contains("src/logger.rs"));
    }

    #[test]
    fn test_synthesize_from_heuristic() {
        let heuristic = "[Heuristic] Project: anukriti Messages: 35 (17 user) Tools: Agent, Bash, Edit, Glob, Grep, Read";
        let story = synthesize_story_from_heuristic(heuristic, "anukriti");
        assert!(!story.is_empty());
        assert!(story.contains("anukriti"));
    }

    #[test]
    fn test_synthesize_from_heuristic_with_errors() {
        let heuristic = "[Heuristic] Project: test Messages: 10 Tools: Bash Had errors: yes";
        let story = synthesize_story_from_heuristic(heuristic, "test");
        assert!(story.contains("error investigation"));
    }

    #[test]
    fn test_needs_haiku_escalation() {
        // Short V3 + few messages = no haiku needed
        assert!(!needs_haiku(
            Some("## User Request\n\"fix bug\"\n## Solution Pattern\ndone"),
            Some("heuristic"),
            10
        ));
        // No enrichment + many messages = needs haiku
        assert!(needs_haiku(None, None, 20));
        // Long session with short V3 = needs haiku
        assert!(needs_haiku(Some("## User Request\n\"x\""), None, 50));
    }

    #[test]
    fn test_needs_haiku_no_enrichment_few_messages() {
        // No enrichment + few messages = no haiku needed
        assert!(!needs_haiku(None, None, 3));
    }

    #[test]
    fn test_extract_section() {
        let content = "## Header\nline 1\nline 2\n\n## Next\nother";
        let section = extract_section(content, "## Header");
        assert_eq!(section, Some("line 1 line 2".to_string()));
    }

    #[test]
    fn test_extract_section_missing() {
        let section = extract_section("## Other\ncontent", "## Missing");
        assert!(section.is_none());
    }

    #[test]
    fn test_story_capped_at_500_chars() {
        let long_request = "x".repeat(600);
        let v3_content = format!("## User Request\n\"{}\"\n", long_request);
        let story = synthesize_story_from_v3(&v3_content, "proj").unwrap();
        assert!(story.len() <= 500);
    }
}
