use super::compact_dream_enabled_from;

#[test]
fn compact_dream_gate_truth_table() {
    assert!(compact_dream_enabled_from(false, false), "on by default");
    assert!(
        !compact_dream_enabled_from(true, false),
        "CSR_NO_DREAMING disables it"
    );
    assert!(
        !compact_dream_enabled_from(false, true),
        "CSR_NO_DREAM_ON_COMPACT disables it"
    );
    assert!(!compact_dream_enabled_from(true, true));
}
