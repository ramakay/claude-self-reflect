use super::*;

#[test]
fn companion_side_note_does_not_change_the_import_grammar() {
    let text = "My own words.\n\nHere is a note offered by CSR (claude-self-reflect):\n> A side note.\n\nMy reply.";
    assert_eq!(strip_csr_forwards(text), None);
    assert_eq!(
        strip_companion_forwards(text),
        Some("My own words.\n\nMy reply.".into())
    );
}
