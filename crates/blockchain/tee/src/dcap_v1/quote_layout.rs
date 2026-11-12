//! SGX quote v3 layout checks for DCAP V1.
//!
//! This module checks the outer length, the header profile and the
//! authentication data layout of a quote. It does not verify signatures.

use outbe_primitives::tee_attestation_v1::TeePolicyV1;

use crate::byte_cursor::ByteCursor;
use crate::dcap_protocol::DcapRejectCodeV1;

const QUOTE_AUTHENTICATION_DATA_LENGTH_OFFSET: usize = 432;
const QUOTE_AUTHENTICATION_DATA_OFFSET: usize = 436;
const QUOTE_SIGNATURE_BYTES: usize = 64;
const ATTESTATION_PUBLIC_KEY_BYTES: usize = 64;
const QE_REPORT_BYTES: usize = 384;
const QE_REPORT_SIGNATURE_BYTES: usize = 64;

pub(super) fn validate_quote_outer_length(quote: &[u8]) -> Result<(), DcapRejectCodeV1> {
    let declared = quote
        .get(
            QUOTE_AUTHENTICATION_DATA_LENGTH_OFFSET
                ..QUOTE_AUTHENTICATION_DATA_LENGTH_OFFSET + size_of::<u32>(),
        )
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(DcapRejectCodeV1::QuoteMalformed)?;
    let expected = QUOTE_AUTHENTICATION_DATA_OFFSET
        .checked_add(usize::try_from(declared).map_err(|_| DcapRejectCodeV1::QuoteMalformed)?)
        .ok_or(DcapRejectCodeV1::QuoteMalformed)?;
    if quote.len() != expected {
        return Err(DcapRejectCodeV1::QuoteMalformed);
    }
    Ok(())
}

pub(super) fn validate_quote_profile(
    quote: &[u8],
    policy: &TeePolicyV1,
) -> Result<(), DcapRejectCodeV1> {
    if QuoteProfile::read(quote)? != QuoteProfile::required_by(policy) {
        return Err(DcapRejectCodeV1::QuoteProfileMismatch);
    }
    Ok(())
}

/// Quote header fields that the active policy fixes.
#[derive(PartialEq, Eq)]
struct QuoteProfile {
    version: u16,
    attestation_key_type: u16,
    tee_type: u32,
    qe_vendor_id: [u8; 16],
}

impl QuoteProfile {
    /// Read all fields before any comparison. A short header is malformed
    /// even when a field that was read does not match the policy.
    fn read(quote: &[u8]) -> Result<Self, DcapRejectCodeV1> {
        Ok(Self {
            version: read_u16(quote, 0)?,
            attestation_key_type: read_u16(quote, 2)?,
            tee_type: read_u32(quote, 4)?,
            qe_vendor_id: quote
                .get(12..28)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or(DcapRejectCodeV1::QuoteMalformed)?,
        })
    }

    const fn required_by(policy: &TeePolicyV1) -> Self {
        Self {
            version: policy.quote_version,
            attestation_key_type: policy.attestation_key_type,
            tee_type: policy.tee_type,
            qe_vendor_id: policy.qe_vendor_id,
        }
    }
}

pub(super) fn parse_quote_authentication_data(
    quote: &[u8],
) -> Result<QuoteAuthenticationData<'_>, DcapRejectCodeV1> {
    let mut cursor = ByteCursor::new(
        quote
            .get(QUOTE_AUTHENTICATION_DATA_OFFSET..)
            .ok_or(DcapRejectCodeV1::QuoteMalformed)?,
        |_| DcapRejectCodeV1::QuoteMalformed,
    );
    cursor.take(QUOTE_SIGNATURE_BYTES)?;
    cursor.take(ATTESTATION_PUBLIC_KEY_BYTES)?;
    cursor.take(QE_REPORT_BYTES)?;
    cursor.take(QE_REPORT_SIGNATURE_BYTES)?;
    let qe_authentication_data_len = usize::from(u16::from_le_bytes(cursor.array()?));
    cursor.take(qe_authentication_data_len)?;
    let certification_data_type = u16::from_le_bytes(cursor.array()?);
    let certification_data_len = usize::try_from(u32::from_le_bytes(cursor.array()?))
        .map_err(|_| DcapRejectCodeV1::QuoteMalformed)?;
    let certification_data = cursor.take(certification_data_len)?;
    cursor.finish()?;
    Ok(QuoteAuthenticationData {
        certification_data_type,
        certification_data,
    })
}

pub(super) struct QuoteAuthenticationData<'a> {
    pub(super) certification_data_type: u16,
    pub(super) certification_data: &'a [u8],
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, DcapRejectCodeV1> {
    bytes
        .get(offset..offset + size_of::<u16>())
        .and_then(|value| value.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or(DcapRejectCodeV1::QuoteMalformed)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, DcapRejectCodeV1> {
    bytes
        .get(offset..offset + size_of::<u32>())
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(DcapRejectCodeV1::QuoteMalformed)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;
    use outbe_primitives::tee_attestation_v1::PlatformTcbStatusSetV1;

    use super::*;

    fn quote_profile_policy() -> TeePolicyV1 {
        TeePolicyV1 {
            policy_version: 1,
            chain_id: [0; 32],
            genesis_hash: B256::ZERO,
            activation_height: 1,
            predecessor_policy_hash: B256::ZERO,
            attestation_mode: outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired,
            intel_root_der_hash: B256::ZERO,
            quote_version: 3,
            tee_type: 0,
            attestation_key_type: 2,
            qe_vendor_id: [0x93; 16],
            certification_data_type: 5,
            tcb_info_schema_version: 3,
            qe_identity_schema_version: 2,
            minimum_tcb_evaluation_data_number: 1,
            accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOnly,
            accepted_qe_tcb_status: outbe_primitives::tee_attestation_v1::QvlTcbStatusV1::UpToDate,
            minimum_lease: 1,
            maximum_lease: 2,
            collateral_margin: 0,
            resource_schedule_hash: B256::ZERO,
            measurement_rules: Vec::new(),
        }
    }

    fn quote_header(version: u16, key_type: u16, tee_type: u32, vendor: [u8; 16]) -> Vec<u8> {
        let mut quote = Vec::new();
        quote.extend_from_slice(&version.to_le_bytes());
        quote.extend_from_slice(&key_type.to_le_bytes());
        quote.extend_from_slice(&tee_type.to_le_bytes());
        quote.extend_from_slice(&[0xee; 4]);
        quote.extend_from_slice(&vendor);
        quote
    }

    #[test]
    fn quote_profile_compares_all_four_header_fields_after_reading_them() {
        let policy = quote_profile_policy();
        assert_eq!(
            validate_quote_profile(&quote_header(3, 2, 0, [0x93; 16]), &policy),
            Ok(())
        );
        for quote in [
            quote_header(4, 2, 0, [0x93; 16]),
            quote_header(3, 3, 0, [0x93; 16]),
            quote_header(3, 2, 0x81, [0x93; 16]),
            quote_header(3, 2, 0, [0x94; 16]),
        ] {
            assert_eq!(
                validate_quote_profile(&quote, &policy),
                Err(DcapRejectCodeV1::QuoteProfileMismatch)
            );
        }
        let mut short = quote_header(4, 3, 0x81, [0x94; 16]);
        short.pop();
        assert_eq!(
            validate_quote_profile(&short, &policy),
            Err(DcapRejectCodeV1::QuoteMalformed)
        );
        assert_eq!(
            validate_quote_profile(&short[..7], &policy),
            Err(DcapRejectCodeV1::QuoteMalformed)
        );
    }
}
