//! Consensus-facing DCAP verification for attestation protocol V1.
//!
//! Callers supply only canonical evidence, the active policy and consensus
//! time. Quote grammar, collateral adaptation, native QVL invocation and
//! policy mapping remain private implementation details.

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{AttestationEvidenceV1, DcapEvidenceV1, TeePolicyV1};

const QUOTE_AUTHENTICATION_DATA_LENGTH_OFFSET: usize = 432;
const QUOTE_AUTHENTICATION_DATA_OFFSET: usize = 436;
const QUOTE_SIGNATURE_BYTES: usize = 64;
const ATTESTATION_PUBLIC_KEY_BYTES: usize = 64;
const QE_REPORT_BYTES: usize = 384;
const QE_REPORT_SIGNATURE_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DcapPckCaV1 {
    Processor = 0x01,
    Platform = 0x02,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DcapPlatformTcbStatusV1 {
    UpToDate = 0x01,
    SWHardeningNeeded = 0x02,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DcapVerdictV1 {
    pub mrenclave: B256,
    pub mrsigner: B256,
    pub isv_prod_id: u16,
    pub isv_svn: u16,
    pub pck_ca: DcapPckCaV1,
    pub fmspc: [u8; 6],
    pub pce_id: u16,
    pub platform_tcb_status: DcapPlatformTcbStatusV1,
    pub advisory_ids: Vec<String>,
    pub tcb_evaluation_data_number: u32,
    pub qe_tcb_evaluation_data_number: u32,
    pub collateral_valid_until: u64,
}

/// Stable consensus-visible rejection codes.
///
/// Numeric values are grouped in verification order. Native error strings,
/// addresses and implementation-specific result values never cross this
/// interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum DcapRejectCodeV1 {
    EvidenceNonCanonical = 0x0101,
    PolicyNonCanonical = 0x0102,
    PolicyBindingMismatch = 0x0103,
    TimestampInvalid = 0x0104,
    QuoteMalformed = 0x0201,
    QuoteProfileMismatch = 0x0202,
    QuoteCertificationDataMismatch = 0x0203,
    ReportDataMismatch = 0x0204,
    CollateralNonCanonical = 0x0301,
    IntelRootMismatch = 0x0302,
    PlatformIdentityMismatch = 0x0303,
    CollateralNotYetValid = 0x0304,
    CollateralExpired = 0x0305,
    NativeVerifierUnavailable = 0x0401,
    NativeVerificationFailed = 0x0402,
    NativeOutputMalformed = 0x0403,
    PlatformTcbRejected = 0x0501,
    QeTcbRejected = 0x0502,
    TcbEvaluationNumberTooLow = 0x0503,
    MeasurementRejected = 0x0601,
}

impl DcapRejectCodeV1 {
    pub const fn code(self) -> u16 {
        self as u16
    }
}

/// Verify one canonical DCAP evidence value using only consensus inputs.
pub fn verify_dcap_evidence(
    evidence: &DcapEvidenceV1,
    policy: &TeePolicyV1,
    block_timestamp: u64,
) -> Result<DcapVerdictV1, DcapRejectCodeV1> {
    AttestationEvidenceV1::Dcap(evidence.clone())
        .encode_canonical()
        .map_err(|_| DcapRejectCodeV1::EvidenceNonCanonical)?;
    policy
        .encode_canonical()
        .map_err(|_| DcapRejectCodeV1::PolicyNonCanonical)?;
    let policy_hash = policy
        .policy_hash()
        .map_err(|_| DcapRejectCodeV1::PolicyNonCanonical)?;
    if evidence.intent.chain_id != policy.chain_id
        || evidence.intent.genesis_hash != policy.genesis_hash
        || evidence.intent.policy_hash != policy_hash
    {
        return Err(DcapRejectCodeV1::PolicyBindingMismatch);
    }
    if block_timestamp > i64::MAX as u64 {
        return Err(DcapRejectCodeV1::TimestampInvalid);
    }

    validate_quote_outer_length(&evidence.quote)?;
    validate_quote_profile(&evidence.quote, policy)?;
    let quote_authentication = parse_quote_authentication_data(&evidence.quote)?;
    let submitted_pck_chain = evidence
        .components
        .first()
        .ok_or(DcapRejectCodeV1::EvidenceNonCanonical)?;
    if quote_authentication.certification_data_type != policy.certification_data_type
        || quote_authentication.certification_data != submitted_pck_chain.bytes
    {
        return Err(DcapRejectCodeV1::QuoteCertificationDataMismatch);
    }
    let measurements = crate::quote::parse_quote_measurements(&evidence.quote)
        .map_err(|_| DcapRejectCodeV1::QuoteMalformed)?;
    let expected_report_data = evidence
        .intent
        .report_data()
        .map_err(|_| DcapRejectCodeV1::EvidenceNonCanonical)?;
    if measurements.report_data != expected_report_data {
        return Err(DcapRejectCodeV1::ReportDataMismatch);
    }

    // The remaining grammar, canonical-collateral adapter and native-QVL
    // mapping are added by the following I1 tracer bullets. Until then the
    // incomplete positive path is explicitly fail-closed.
    Err(DcapRejectCodeV1::NativeVerifierUnavailable)
}

fn validate_quote_outer_length(quote: &[u8]) -> Result<(), DcapRejectCodeV1> {
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

fn validate_quote_profile(quote: &[u8], policy: &TeePolicyV1) -> Result<(), DcapRejectCodeV1> {
    let version = read_u16(quote, 0)?;
    let attestation_key_type = read_u16(quote, 2)?;
    let tee_type = read_u32(quote, 4)?;
    let qe_vendor_id: [u8; 16] = quote
        .get(12..28)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(DcapRejectCodeV1::QuoteMalformed)?;
    if version != policy.quote_version
        || attestation_key_type != policy.attestation_key_type
        || tee_type != policy.tee_type
        || qe_vendor_id != policy.qe_vendor_id
    {
        return Err(DcapRejectCodeV1::QuoteProfileMismatch);
    }
    Ok(())
}

fn parse_quote_authentication_data(
    quote: &[u8],
) -> Result<QuoteAuthenticationData<'_>, DcapRejectCodeV1> {
    let mut cursor = QuoteCursor::new(
        quote
            .get(QUOTE_AUTHENTICATION_DATA_OFFSET..)
            .ok_or(DcapRejectCodeV1::QuoteMalformed)?,
    );
    cursor.take(QUOTE_SIGNATURE_BYTES)?;
    cursor.take(ATTESTATION_PUBLIC_KEY_BYTES)?;
    cursor.take(QE_REPORT_BYTES)?;
    cursor.take(QE_REPORT_SIGNATURE_BYTES)?;
    let qe_authentication_data_len = usize::from(cursor.u16()?);
    cursor.take(qe_authentication_data_len)?;
    let certification_data_type = cursor.u16()?;
    let certification_data_len =
        usize::try_from(cursor.u32()?).map_err(|_| DcapRejectCodeV1::QuoteMalformed)?;
    let certification_data = cursor.take(certification_data_len)?;
    cursor.finish()?;
    Ok(QuoteAuthenticationData {
        certification_data_type,
        certification_data,
    })
}

struct QuoteAuthenticationData<'a> {
    certification_data_type: u16,
    certification_data: &'a [u8],
}

struct QuoteCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> QuoteCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], DcapRejectCodeV1> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(DcapRejectCodeV1::QuoteMalformed)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(DcapRejectCodeV1::QuoteMalformed)?;
        self.offset = end;
        Ok(value)
    }

    fn u16(&mut self) -> Result<u16, DcapRejectCodeV1> {
        self.take(size_of::<u16>())?
            .try_into()
            .map(u16::from_le_bytes)
            .map_err(|_| DcapRejectCodeV1::QuoteMalformed)
    }

    fn u32(&mut self) -> Result<u32, DcapRejectCodeV1> {
        self.take(size_of::<u32>())?
            .try_into()
            .map(u32::from_le_bytes)
            .map_err(|_| DcapRejectCodeV1::QuoteMalformed)
    }

    fn finish(self) -> Result<(), DcapRejectCodeV1> {
        if self.offset != self.bytes.len() {
            return Err(DcapRejectCodeV1::QuoteMalformed);
        }
        Ok(())
    }
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
