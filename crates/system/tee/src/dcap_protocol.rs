//! Stable, native-verifier-independent DCAP protocol values.
//!
//! These values cross the Gramine enclave seam. The node can decode and
//! validate the canonical outcome without linking Intel QVL; only the enclave
//! compiles the native verifier implementation.

use alloy_primitives::{keccak256, B256};
use outbe_primitives::tee_attestation_v1::{MAX_ATTESTATION_EVIDENCE_BYTES, MAX_TEE_POLICY_BYTES};

const DCAP_VERIFICATION_REQUEST_DOMAIN_V1: &[u8] = b"outbe/tee/dcap-verification-request/v1";
const DCAP_VERIFICATION_ATTESTATION_DOMAIN_V1: &[u8] =
    b"outbe/tee/dcap-verification-attestation/v1";
const DCAP_EVIDENCE_DOMAIN_V1: &[u8] = b"outbe/tee/dcap-evidence/v1";

/// Maximum plaintext payload in one upload chunk. Postcard metadata and Noise
/// authentication remain safely below the shared 64 KiB frame ceiling.
pub const MAX_DCAP_VERIFICATION_CHUNK_BYTES: usize = 60 * 1024;

/// Upper bound for the canonical stable verdict returned by the enclave.
/// Intel QVL 1.26 exposes at most 450 advisory bytes; 1 KiB leaves bounded
/// framing headroom without allowing an unbounded response.
pub const MAX_DCAP_VERDICT_BYTES: usize = 1024;

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

impl DcapVerdictV1 {
    pub fn encode_canonical(&self) -> Result<Vec<u8>, DcapRejectCodeV1> {
        let advisory_count = u16::try_from(self.advisory_ids.len())
            .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?;
        let mut encoded = Vec::new();
        encoded.push(1);
        encoded.extend_from_slice(self.mrenclave.as_slice());
        encoded.extend_from_slice(self.mrsigner.as_slice());
        encoded.extend_from_slice(&self.isv_prod_id.to_be_bytes());
        encoded.extend_from_slice(&self.isv_svn.to_be_bytes());
        encoded.push(self.pck_ca as u8);
        encoded.extend_from_slice(&self.fmspc);
        encoded.extend_from_slice(&self.pce_id.to_be_bytes());
        encoded.push(self.platform_tcb_status as u8);
        encoded.extend_from_slice(&self.tcb_evaluation_data_number.to_be_bytes());
        encoded.extend_from_slice(&self.qe_tcb_evaluation_data_number.to_be_bytes());
        encoded.extend_from_slice(&self.collateral_valid_until.to_be_bytes());
        encoded.extend_from_slice(&advisory_count.to_be_bytes());
        for advisory in &self.advisory_ids {
            validate_advisory(advisory)?;
            let len = u16::try_from(advisory.len())
                .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?;
            encoded.extend_from_slice(&len.to_be_bytes());
            encoded.extend_from_slice(advisory.as_bytes());
        }
        if encoded.len() > MAX_DCAP_VERDICT_BYTES {
            return Err(DcapRejectCodeV1::NativeOutputMalformed);
        }
        Ok(encoded)
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, DcapRejectCodeV1> {
        if input.len() > MAX_DCAP_VERDICT_BYTES {
            return Err(DcapRejectCodeV1::NativeOutputMalformed);
        }
        let mut decoder = Decoder::new(input);
        if decoder.u8()? != 1 {
            return Err(DcapRejectCodeV1::NativeOutputMalformed);
        }
        let mrenclave = B256::from_slice(decoder.take(32)?);
        let mrsigner = B256::from_slice(decoder.take(32)?);
        let isv_prod_id = decoder.u16()?;
        let isv_svn = decoder.u16()?;
        let pck_ca = match decoder.u8()? {
            0x01 => DcapPckCaV1::Processor,
            0x02 => DcapPckCaV1::Platform,
            _ => return Err(DcapRejectCodeV1::NativeOutputMalformed),
        };
        let fmspc = decoder
            .take(6)?
            .try_into()
            .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?;
        let pce_id = decoder.u16()?;
        let platform_tcb_status = match decoder.u8()? {
            0x01 => DcapPlatformTcbStatusV1::UpToDate,
            0x02 => DcapPlatformTcbStatusV1::SWHardeningNeeded,
            _ => return Err(DcapRejectCodeV1::NativeOutputMalformed),
        };
        let tcb_evaluation_data_number = decoder.u32()?;
        let qe_tcb_evaluation_data_number = decoder.u32()?;
        let collateral_valid_until = decoder.u64()?;
        let advisory_count = usize::from(decoder.u16()?);
        let mut advisory_ids = Vec::with_capacity(advisory_count);
        for _ in 0..advisory_count {
            let len = usize::from(decoder.u16()?);
            let advisory = std::str::from_utf8(decoder.take(len)?)
                .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?
                .to_owned();
            validate_advisory(&advisory)?;
            advisory_ids.push(advisory);
        }
        decoder.finish()?;
        Ok(Self {
            mrenclave,
            mrsigner,
            isv_prod_id,
            isv_svn,
            pck_ca,
            fmspc,
            pce_id,
            platform_tcb_status,
            advisory_ids,
            tcb_evaluation_data_number,
            qe_tcb_evaluation_data_number,
            collateral_valid_until,
        })
    }
}

fn validate_advisory(advisory: &str) -> Result<(), DcapRejectCodeV1> {
    if advisory.is_empty()
        || !advisory
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(DcapRejectCodeV1::NativeOutputMalformed);
    }
    Ok(())
}

/// Stable consensus-visible rejection codes.
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

impl TryFrom<u16> for DcapRejectCodeV1 {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Ok(match value {
            0x0101 => Self::EvidenceNonCanonical,
            0x0102 => Self::PolicyNonCanonical,
            0x0103 => Self::PolicyBindingMismatch,
            0x0104 => Self::TimestampInvalid,
            0x0201 => Self::QuoteMalformed,
            0x0202 => Self::QuoteProfileMismatch,
            0x0203 => Self::QuoteCertificationDataMismatch,
            0x0204 => Self::ReportDataMismatch,
            0x0301 => Self::CollateralNonCanonical,
            0x0302 => Self::IntelRootMismatch,
            0x0303 => Self::PlatformIdentityMismatch,
            0x0304 => Self::CollateralNotYetValid,
            0x0305 => Self::CollateralExpired,
            0x0401 => Self::NativeVerifierUnavailable,
            0x0402 => Self::NativeVerificationFailed,
            0x0403 => Self::NativeOutputMalformed,
            0x0501 => Self::PlatformTcbRejected,
            0x0502 => Self::QeTcbRejected,
            0x0503 => Self::TcbEvaluationNumberTooLow,
            0x0601 => Self::MeasurementRejected,
            _ => return Err(()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DcapVerificationOutcomeV1 {
    Accepted(DcapVerdictV1),
    Rejected(DcapRejectCodeV1),
}

impl DcapVerificationOutcomeV1 {
    pub fn encode_canonical(&self) -> Result<Vec<u8>, DcapRejectCodeV1> {
        match self {
            Self::Accepted(verdict) => {
                let verdict = verdict.encode_canonical()?;
                let len = u32::try_from(verdict.len())
                    .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?;
                let mut encoded = Vec::with_capacity(6 + verdict.len());
                encoded.extend_from_slice(&[1, 1]);
                encoded.extend_from_slice(&len.to_be_bytes());
                encoded.extend_from_slice(&verdict);
                Ok(encoded)
            }
            Self::Rejected(code) => Ok(vec![1, 2, (code.code() >> 8) as u8, code.code() as u8]),
        }
    }

    pub fn decode_canonical(input: &[u8]) -> Result<Self, DcapRejectCodeV1> {
        let mut decoder = Decoder::new(input);
        if decoder.u8()? != 1 {
            return Err(DcapRejectCodeV1::NativeOutputMalformed);
        }
        let outcome = match decoder.u8()? {
            1 => {
                let len = usize::try_from(decoder.u32()?)
                    .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?;
                Self::Accepted(DcapVerdictV1::decode_canonical(decoder.take(len)?)?)
            }
            2 => Self::Rejected(
                DcapRejectCodeV1::try_from(decoder.u16()?)
                    .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?,
            ),
            _ => return Err(DcapRejectCodeV1::NativeOutputMalformed),
        };
        decoder.finish()?;
        Ok(outcome)
    }
}

/// Commit to the exact verifier inputs supplied over the authenticated local
/// channel. Length prefixes make the three fields unambiguous.
pub fn dcap_verification_request_hash(
    evidence: &[u8],
    policy: &[u8],
    block_timestamp: u64,
) -> Result<B256, DcapRejectCodeV1> {
    if evidence.len() > MAX_ATTESTATION_EVIDENCE_BYTES {
        return Err(DcapRejectCodeV1::EvidenceNonCanonical);
    }
    if policy.len() > MAX_TEE_POLICY_BYTES {
        return Err(DcapRejectCodeV1::PolicyNonCanonical);
    }
    let evidence_len =
        u32::try_from(evidence.len()).map_err(|_| DcapRejectCodeV1::EvidenceNonCanonical)?;
    let policy_len =
        u32::try_from(policy.len()).map_err(|_| DcapRejectCodeV1::PolicyNonCanonical)?;
    let mut preimage = Vec::with_capacity(
        DCAP_VERIFICATION_REQUEST_DOMAIN_V1.len() + evidence.len() + policy.len() + 16,
    );
    preimage.extend_from_slice(DCAP_VERIFICATION_REQUEST_DOMAIN_V1);
    preimage.extend_from_slice(&evidence_len.to_be_bytes());
    preimage.extend_from_slice(evidence);
    preimage.extend_from_slice(&policy_len.to_be_bytes());
    preimage.extend_from_slice(policy);
    preimage.extend_from_slice(&block_timestamp.to_be_bytes());
    Ok(keccak256(preimage))
}

/// Domain-separated hash of the exact canonical evidence bytes used for
/// current-binding idempotency. A new quote or collateral set is not an exact
/// replay even when it carries the same registration intent.
pub fn dcap_evidence_hash_v1(evidence: &[u8]) -> Result<B256, DcapRejectCodeV1> {
    if evidence.len() > MAX_ATTESTATION_EVIDENCE_BYTES {
        return Err(DcapRejectCodeV1::EvidenceNonCanonical);
    }
    let len = u32::try_from(evidence.len()).map_err(|_| DcapRejectCodeV1::EvidenceNonCanonical)?;
    let mut preimage = Vec::with_capacity(DCAP_EVIDENCE_DOMAIN_V1.len() + evidence.len() + 4);
    preimage.extend_from_slice(DCAP_EVIDENCE_DOMAIN_V1);
    preimage.extend_from_slice(&len.to_be_bytes());
    preimage.extend_from_slice(evidence);
    Ok(keccak256(preimage))
}

/// Local-only Ed25519 signature preimage binding an enclave outcome to the
/// exact request commitment. It is verified by the node and never enters
/// calldata, state or events.
pub fn dcap_verification_attestation_preimage(
    request_hash: B256,
    outcome: &[u8],
) -> Result<Vec<u8>, DcapRejectCodeV1> {
    if outcome.len() > MAX_DCAP_VERDICT_BYTES + 6 {
        return Err(DcapRejectCodeV1::NativeOutputMalformed);
    }
    let len = u32::try_from(outcome.len()).map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?;
    let mut preimage =
        Vec::with_capacity(DCAP_VERIFICATION_ATTESTATION_DOMAIN_V1.len() + outcome.len() + 36);
    preimage.extend_from_slice(DCAP_VERIFICATION_ATTESTATION_DOMAIN_V1);
    preimage.extend_from_slice(request_hash.as_slice());
    preimage.extend_from_slice(&len.to_be_bytes());
    preimage.extend_from_slice(outcome);
    Ok(preimage)
}

struct Decoder<'a> {
    input: &'a [u8],
    cursor: usize,
}

impl<'a> Decoder<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, cursor: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], DcapRejectCodeV1> {
        let end = self
            .cursor
            .checked_add(len)
            .ok_or(DcapRejectCodeV1::NativeOutputMalformed)?;
        let value = self
            .input
            .get(self.cursor..end)
            .ok_or(DcapRejectCodeV1::NativeOutputMalformed)?;
        self.cursor = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, DcapRejectCodeV1> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, DcapRejectCodeV1> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?,
        ))
    }

    fn u32(&mut self) -> Result<u32, DcapRejectCodeV1> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, DcapRejectCodeV1> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?,
        ))
    }

    fn finish(self) -> Result<(), DcapRejectCodeV1> {
        if self.cursor != self.input.len() {
            return Err(DcapRejectCodeV1::NativeOutputMalformed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict() -> DcapVerdictV1 {
        DcapVerdictV1 {
            mrenclave: B256::repeat_byte(0x11),
            mrsigner: B256::repeat_byte(0x22),
            isv_prod_id: 7,
            isv_svn: 4,
            pck_ca: DcapPckCaV1::Processor,
            fmspc: [0x33; 6],
            pce_id: 2,
            platform_tcb_status: DcapPlatformTcbStatusV1::SWHardeningNeeded,
            advisory_ids: vec!["INTEL-SA-00001".to_string()],
            tcb_evaluation_data_number: 19,
            qe_tcb_evaluation_data_number: 18,
            collateral_valid_until: 1_800_000_000,
        }
    }

    #[test]
    fn canonical_outcome_round_trips_and_rejects_trailing_or_unknown_codes() {
        for outcome in [
            DcapVerificationOutcomeV1::Accepted(verdict()),
            DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::PlatformTcbRejected),
        ] {
            let encoded = outcome.encode_canonical().unwrap();
            assert_eq!(
                DcapVerificationOutcomeV1::decode_canonical(&encoded),
                Ok(outcome)
            );
            let mut trailing = encoded;
            trailing.push(0);
            assert_eq!(
                DcapVerificationOutcomeV1::decode_canonical(&trailing),
                Err(DcapRejectCodeV1::NativeOutputMalformed)
            );
        }
        assert_eq!(
            DcapVerificationOutcomeV1::decode_canonical(&[1, 2, 0xFF, 0xFF]),
            Err(DcapRejectCodeV1::NativeOutputMalformed)
        );
    }

    #[test]
    fn request_and_evidence_commitments_bind_every_exact_input() {
        let evidence = [0x11, 0x12, 0x13];
        let policy = [0x21, 0x22];
        let timestamp = 1_700_000_000;
        let request = dcap_verification_request_hash(&evidence, &policy, timestamp).unwrap();
        assert_ne!(
            request,
            dcap_verification_request_hash(&[0x11, 0x12, 0x14], &policy, timestamp).unwrap()
        );
        assert_ne!(
            request,
            dcap_verification_request_hash(&evidence, &[0x21, 0x23], timestamp).unwrap()
        );
        assert_ne!(
            request,
            dcap_verification_request_hash(&evidence, &policy, timestamp + 1).unwrap()
        );
        assert_ne!(dcap_evidence_hash_v1(&evidence).unwrap(), B256::ZERO);
        assert_ne!(
            dcap_evidence_hash_v1(&evidence).unwrap(),
            dcap_evidence_hash_v1(&[0x11, 0x12, 0x14]).unwrap()
        );
        let outcome = DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::PlatformTcbRejected)
            .encode_canonical()
            .unwrap();
        let preimage = dcap_verification_attestation_preimage(request, &outcome).unwrap();
        assert!(preimage.ends_with(&outcome));
        assert!(preimage
            .windows(32)
            .any(|window| window == request.as_slice()));
    }
}
