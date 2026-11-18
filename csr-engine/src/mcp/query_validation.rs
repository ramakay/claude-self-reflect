/// Reject blank search input before any database, embedding or proxy work.
pub(super) fn validate(value: &str, field: &str) -> Result<(), rmcp::ErrorData> {
    if value.trim().is_empty() {
        return Err(rmcp::ErrorData::invalid_params(
            format!("{field} must not be empty or blank"),
            None,
        ));
    }
    Ok(())
}
