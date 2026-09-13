#[cfg(feature = "mock")]
pub(in crate::transport) fn synthetic_dcap_quote(
    report_data: &[u8; 64],
) -> Result<Vec<u8>, String> {
    let mut quote = vec![0_u8; outbe_tee::quote::MIN_QUOTE_LEN];
    let report_data_offset = quote
        .len()
        .checked_sub(report_data.len())
        .ok_or_else(|| "synthetic quote REPORT_DATA offset underflow".to_string())?;
    quote[report_data_offset..].copy_from_slice(report_data);
    Ok(quote)
}

/// Re-parse the quote returned by Gramine before exposing it to NodeHost. This
/// keeps a stale/misbound device response from being paired with the requested
/// canonical intent even though the later consensus QVL remains authoritative.
pub(in crate::transport) fn validate_generated_quote_binding(
    expected_report_data: [u8; 64],
    quote: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let parsed = outbe_tee::quote::parse_quote_measurements(&quote)?;
    if parsed.report_data != expected_report_data {
        return Err("generated DCAP quote REPORT_DATA does not match requested intent".to_string());
    }
    Ok(quote)
}
