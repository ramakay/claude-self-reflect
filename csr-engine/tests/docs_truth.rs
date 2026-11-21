//! The docs tell what the product does. Two checks:
//!
//! 1. `--help` of `csr-engine` and of every subcommand, at any depth, holds no internal
//!    label (an S-number, a WP-letter or a B-number). Those mean something to the people who
//!    built the product and nothing to the person who runs it.
//! 2. The README names every card the pane draws. The list of cards is the pane's own
//!    registry, so a card added to the pane without a line in the README fails here.

use std::process::{Command, Stdio};

use csr_engine::companion::render::instrument::CARD_REGISTRY;

/// `--help` of the binary with the given subcommand path.
fn help_of(path: &[String]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .args(path)
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .expect("run csr-engine --help");
    assert!(
        out.status.success(),
        "`csr-engine {} --help` exited {:?}",
        path.join(" "),
        out.status.code()
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The subcommand names a help text lists under `Commands:`, without `help`.
fn subcommands(help: &str) -> Vec<String> {
    let mut names = vec![];
    let mut in_commands = false;
    for line in help.lines() {
        if line.starts_with("Commands:") {
            in_commands = true;
        } else if in_commands {
            if line.is_empty() || !line.starts_with(' ') {
                in_commands = false;
            } else if line.starts_with("  ") && !line.starts_with("   ") {
                if let Some(name) = line.split_whitespace().next() {
                    if name != "help" {
                        names.push(name.to_owned());
                    }
                }
            }
        }
    }
    names
}

fn walk(path: &mut Vec<String>, seen: &mut Vec<(String, String)>) {
    let help = help_of(path);
    for name in subcommands(&help) {
        path.push(name);
        walk(path, seen);
        path.pop();
    }
    seen.push((path.join(" "), help));
}

#[test]
fn help_holds_no_internal_label() {
    let label = regex::Regex::new(r"\b(?:S[0-9]+|WP-?[A-Z0-9]+|B[0-9]+)\b").unwrap();
    let mut seen = vec![];
    walk(&mut vec![], &mut seen);
    assert!(
        seen.len() > 30,
        "expected the top level and more than 30 subcommands, walked {}",
        seen.len()
    );
    let mut hits = vec![];
    for (path, help) in &seen {
        for line in help.lines() {
            if let Some(m) = label.find(line) {
                hits.push(format!(
                    "`csr-engine {path} --help`: {}: {line}",
                    m.as_str()
                ));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "internal labels in --help:\n{}",
        hits.join("\n")
    );
}

#[test]
fn readme_names_every_card_the_pane_draws() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../README.md"))
        .expect("read README.md");
    // Region names that are layout parts, not cards a person sees.
    let not_cards = ["hero", "stack", "figure facts", "notice", "agents", "help"];
    let mut missing = vec![];
    let mut checked = 0;
    for spec in CARD_REGISTRY {
        for title in spec.titles {
            if not_cards.contains(title) {
                continue;
            }
            checked += 1;
            if !readme.contains(title) {
                missing.push(*title);
            }
        }
    }
    // The card that stands in for "What confused the agent" when the dreamer has a reading.
    checked += 1;
    if !readme.contains(csr_engine::companion::same_page::TITLE) {
        missing.push(csr_engine::companion::same_page::TITLE);
    }
    assert!(
        checked >= 10,
        "the registry lists fewer cards than expected: {checked}"
    );
    assert!(
        missing.is_empty(),
        "the README does not name these pane cards: {missing:?}"
    );
}
