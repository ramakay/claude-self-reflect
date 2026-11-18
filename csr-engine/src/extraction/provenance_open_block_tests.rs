use super::*;

fn block() -> String {
    format!("{OPEN_BLOCK_HEADER}\n- Still need to push the branch\n- check receipt:true in the compact log\n")
}

#[test]
fn the_open_block_is_an_emission_however_it_is_quoted() {
    let block = block();
    assert!(is_csr_emission(&block));
    for wrapped in [
        block
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        format!("- {}", block.trim_end()),
        format!("**{}**\n- x", OPEN_BLOCK_HEADER),
        format!(
            "{}\n{block}",
            "a long preamble of ordinary words ".repeat(40)
        ),
        block.replace('[', "\u{ff3b}").replace(']', "\u{ff3d}"),
        block.replace("Open", "O\u{200b}pen"),
    ] {
        assert!(is_csr_emission(&wrapped), "{wrapped}");
    }
}

#[test]
fn prose_that_only_starts_alike_is_not_an_emission() {
    for prose in [
        "Open [the config file] and check the history, not the tasks.",
        "Open items from the last compact summary: none.",
        "Please open [the list] from the last compact summary.",
    ] {
        assert!(!is_csr_emission(prose), "{prose}");
    }
}

#[test]
fn the_open_block_goes_with_its_bullets_when_a_prompt_is_scrubbed() {
    let prompt = format!("fix the build\n{}\nthen push it", block());
    let kept = strip_csr_emissions(&prompt);
    assert!(!kept.contains("Still need to push"), "{kept}");
    assert!(
        kept.contains("fix the build") && kept.contains("then push it"),
        "{kept}"
    );
}
