use crate::errors::{EnclaveResult, TransportDispatchError};

#[cfg(feature = "mock")]
pub(in crate::transport) fn synthetic_dcap_quote(report_data: &[u8; 64]) -> EnclaveResult<Vec<u8>> {
    let mut quote = vec![0_u8; outbe_tee::quote::MIN_QUOTE_LEN];
    let report_data_offset = quote
        .len()
        .checked_sub(report_data.len())
        .ok_or(TransportDispatchError::SyntheticQuoteOffsetUnderflow)?;
    quote[report_data_offset..].copy_from_slice(report_data);
    Ok(quote)
}

/// Re-parse the quote returned by Gramine before exposing it to NodeHost. This
/// keeps a stale/misbound device response from being paired with the requested
/// canonical intent even though the later consensus QVL remains authoritative.
pub(in crate::transport) fn validate_generated_quote_binding(
    expected_report_data: [u8; 64],
    quote: Vec<u8>,
) -> EnclaveResult<Vec<u8>> {
    let parsed = outbe_tee::quote::parse_quote_measurements(&quote)
        .map_err(TransportDispatchError::detail)?;
    if parsed.report_data != expected_report_data {
        return Err(TransportDispatchError::GeneratedQuoteReportDataMismatch.into());
    }
    Ok(quote)
}
