//! Consensus-facing DCAP verification for attestation protocol V1.
//!
//! Callers supply only canonical evidence, the active policy and consensus
//! time. Quote grammar, collateral adaptation, native QVL invocation and
//! policy mapping remain private implementation details.

use alloy_primitives::B256;
use der::{asn1::AnyRef, Decode as _, Encode as _, Tag, Tagged as _};
use outbe_primitives::tee_attestation_v1::{AttestationEvidenceV1, DcapEvidenceV1, TeePolicyV1};
use pem::{EncodeConfig, LineEnding};
use serde::Deserialize;
use serde_json::value::RawValue;
use sha2::{Digest as _, Sha256};

use crate::native_qvl::{verify_quote_native, NativeDcapCollateral, NativeQvlError};

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
    validate_canonical_pck_certificate_chain(component(evidence, 0)?)?;
    validate_canonical_der_crl(component(evidence, 1)?)?;
    validate_canonical_certificate_chain(component(evidence, 2)?)?;
    validate_canonical_der_crl(component(evidence, 3)?)?;
    validate_canonical_certificate_chain(component(evidence, 5)?)?;
    validate_canonical_certificate_chain(component(evidence, 7)?)?;
    if pck_root_der_hash(component(evidence, 0)?)? != policy.intel_root_der_hash {
        return Err(DcapRejectCodeV1::IntelRootMismatch);
    }
    let tcb_info = parse_signed_tcb_info(component(evidence, 4)?, policy)?;
    let qe_identity = parse_signed_qe_identity(component(evidence, 6)?, policy)?;
    let issue_floor = tcb_info.issue_date.max(qe_identity.issue_date);
    let expiration_ceiling = tcb_info.next_update.min(qe_identity.next_update);
    if block_timestamp < issue_floor {
        return Err(DcapRejectCodeV1::CollateralNotYetValid);
    }
    if block_timestamp >= expiration_ceiling {
        return Err(DcapRejectCodeV1::CollateralExpired);
    }
    if tcb_info.tcb_evaluation_data_number < policy.minimum_tcb_evaluation_data_number
        || qe_identity.tcb_evaluation_data_number < policy.minimum_tcb_evaluation_data_number
    {
        return Err(DcapRejectCodeV1::TcbEvaluationNumberTooLow);
    }
    let native_collateral = NativeDcapCollateral {
        pck_crl_issuer_chain: component(evidence, 2)?,
        root_ca_crl: component(evidence, 3)?,
        pck_crl: component(evidence, 1)?,
        tcb_info_issuer_chain: component(evidence, 5)?,
        tcb_info: component(evidence, 4)?,
        qe_identity_issuer_chain: component(evidence, 7)?,
        qe_identity: component(evidence, 6)?,
    };
    let _native_verdict = verify_quote_native(
        &evidence.quote,
        &native_collateral,
        i64::try_from(block_timestamp).map_err(|_| DcapRejectCodeV1::TimestampInvalid)?,
    )
    .map_err(map_native_error)?;

    // The remaining grammar, canonical-collateral adapter and native-QVL
    // mapping are added by the following I1 tracer bullets. Until then the
    // incomplete positive path is explicitly fail-closed.
    Err(DcapRejectCodeV1::NativeVerifierUnavailable)
}

const fn map_native_error(error: NativeQvlError) -> DcapRejectCodeV1 {
    match error {
        NativeQvlError::InvalidInput => DcapRejectCodeV1::CollateralNonCanonical,
        NativeQvlError::UnsupportedAbi => DcapRejectCodeV1::NativeVerifierUnavailable,
        NativeQvlError::VerificationFailed => DcapRejectCodeV1::NativeVerificationFailed,
        NativeQvlError::UnsupportedResult | NativeQvlError::MalformedSupplemental => {
            DcapRejectCodeV1::NativeOutputMalformed
        }
    }
}

fn component(evidence: &DcapEvidenceV1, index: usize) -> Result<&[u8], DcapRejectCodeV1> {
    evidence
        .components
        .get(index)
        .map(|component| component.bytes.as_slice())
        .ok_or(DcapRejectCodeV1::EvidenceNonCanonical)
}

fn validate_canonical_pck_certificate_chain(bytes: &[u8]) -> Result<(), DcapRejectCodeV1> {
    let canonical_pem = bytes
        .strip_suffix(&[0])
        .ok_or(DcapRejectCodeV1::CollateralNonCanonical)?;
    if canonical_pem.contains(&0) {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    validate_canonical_certificate_chain(canonical_pem)
}

fn pck_root_der_hash(bytes: &[u8]) -> Result<B256, DcapRejectCodeV1> {
    let canonical_pem = bytes
        .strip_suffix(&[0])
        .ok_or(DcapRejectCodeV1::CollateralNonCanonical)?;
    let certificates =
        pem::parse_many(canonical_pem).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    let root = certificates
        .last()
        .ok_or(DcapRejectCodeV1::CollateralNonCanonical)?;
    Ok(B256::from_slice(&Sha256::digest(root.contents())))
}

fn validate_canonical_certificate_chain(bytes: &[u8]) -> Result<(), DcapRejectCodeV1> {
    let certificates =
        pem::parse_many(bytes).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    if certificates.is_empty() {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    let config = EncodeConfig::new().set_line_ending(LineEnding::LF);
    let mut canonical = String::new();
    for certificate in certificates {
        if certificate.tag() != "CERTIFICATE" || certificate.headers().iter().next().is_some() {
            return Err(DcapRejectCodeV1::CollateralNonCanonical);
        }
        let canonical_der = canonical_der_document(certificate.contents())?;
        if canonical_der != certificate.contents() {
            return Err(DcapRejectCodeV1::CollateralNonCanonical);
        }
        canonical.push_str(&pem::encode_config(
            &pem::Pem::new("CERTIFICATE", canonical_der),
            config,
        ));
    }
    if canonical.as_bytes() != bytes {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    Ok(())
}

fn validate_canonical_der_crl(bytes: &[u8]) -> Result<(), DcapRejectCodeV1> {
    let canonical = canonical_der_document(bytes)?;
    if canonical != bytes {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    Ok(())
}

fn canonical_der_document(bytes: &[u8]) -> Result<Vec<u8>, DcapRejectCodeV1> {
    let document = AnyRef::from_der(bytes).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    if document.tag() != Tag::Sequence {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    document
        .to_der()
        .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedTcbInfo<'a> {
    #[serde(borrow, rename = "tcbInfo")]
    body: &'a RawValue,
    signature: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedQeIdentity<'a> {
    #[serde(borrow, rename = "enclaveIdentity")]
    body: &'a RawValue,
    signature: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TcbInfoBody<'a> {
    id: &'a str,
    version: u8,
    issue_date: &'a str,
    next_update: &'a str,
    fmspc: &'a str,
    pce_id: &'a str,
    tcb_evaluation_data_number: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QeIdentityBody<'a> {
    id: &'a str,
    version: u8,
    issue_date: &'a str,
    next_update: &'a str,
    tcb_evaluation_data_number: u32,
}

struct TcbInfoMetadata {
    issue_date: u64,
    next_update: u64,
    #[allow(dead_code)]
    fmspc: [u8; 6],
    #[allow(dead_code)]
    pce_id: u16,
    tcb_evaluation_data_number: u32,
}

struct QeIdentityMetadata {
    issue_date: u64,
    next_update: u64,
    tcb_evaluation_data_number: u32,
}

fn parse_signed_tcb_info(
    bytes: &[u8],
    policy: &TeePolicyV1,
) -> Result<TcbInfoMetadata, DcapRejectCodeV1> {
    let signed: SignedTcbInfo<'_> =
        serde_json::from_slice(bytes).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    validate_signed_json_wrapper(bytes, "tcbInfo", signed.body, signed.signature)?;
    let body: TcbInfoBody<'_> = serde_json::from_str(signed.body.get())
        .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    if body.id != "SGX" || body.version != policy.tcb_info_schema_version {
        return Err(DcapRejectCodeV1::PlatformIdentityMismatch);
    }
    let issue_date = parse_canonical_timestamp(body.issue_date)?;
    let next_update = parse_canonical_timestamp(body.next_update)?;
    if issue_date >= next_update {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    Ok(TcbInfoMetadata {
        issue_date,
        next_update,
        fmspc: decode_upper_hex(body.fmspc)?,
        pce_id: u16::from_be_bytes(decode_upper_hex(body.pce_id)?),
        tcb_evaluation_data_number: body.tcb_evaluation_data_number,
    })
}

fn parse_signed_qe_identity(
    bytes: &[u8],
    policy: &TeePolicyV1,
) -> Result<QeIdentityMetadata, DcapRejectCodeV1> {
    let signed: SignedQeIdentity<'_> =
        serde_json::from_slice(bytes).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    validate_signed_json_wrapper(bytes, "enclaveIdentity", signed.body, signed.signature)?;
    let body: QeIdentityBody<'_> = serde_json::from_str(signed.body.get())
        .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    if body.id != "QE" || body.version != policy.qe_identity_schema_version {
        return Err(DcapRejectCodeV1::QeTcbRejected);
    }
    let issue_date = parse_canonical_timestamp(body.issue_date)?;
    let next_update = parse_canonical_timestamp(body.next_update)?;
    if issue_date >= next_update {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    Ok(QeIdentityMetadata {
        issue_date,
        next_update,
        tcb_evaluation_data_number: body.tcb_evaluation_data_number,
    })
}

fn validate_signed_json_wrapper(
    bytes: &[u8],
    field: &str,
    body: &RawValue,
    signature: &str,
) -> Result<(), DcapRejectCodeV1> {
    if !body.get().starts_with('{')
        || !body.get().ends_with('}')
        || signature.len() != 128
        || !signature
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    let canonical = format!(r#"{{"{field}":{},"signature":"{signature}"}}"#, body.get());
    if canonical.as_bytes() != bytes {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    Ok(())
}

fn parse_canonical_timestamp(value: &str) -> Result<u64, DcapRejectCodeV1> {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
        || bytes.iter().enumerate().any(|(index, byte)| {
            !matches!(index, 4 | 7 | 10 | 13 | 16 | 19) && !byte.is_ascii_digit()
        })
    {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    let timestamp =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?
            .unix_timestamp();
    u64::try_from(timestamp).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)
}

fn decode_upper_hex<const N: usize>(value: &str) -> Result<[u8; N], DcapRejectCodeV1> {
    if value.len() != N * 2 {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    let mut decoded = [0; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = upper_hex_nibble(pair[0])?;
        let low = upper_hex_nibble(pair[1])?;
        decoded[index] = (high << 4) | low;
    }
    Ok(decoded)
}

fn upper_hex_nibble(value: u8) -> Result<u8, DcapRejectCodeV1> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(DcapRejectCodeV1::CollateralNonCanonical),
    }
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
