use alloy_primitives::B256;

use super::ParticipantAnnounce;

/// Optional enclave telemetry retained in the canary RPC snapshot schema.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveHealthStatusV1 {
    pub uptime_s: u64,
    pub offer_key_ready: bool,
    pub heap: EnclaveHeapUsageV1,
    pub requests: EnclaveRequestCountsV1,
    pub classes: EnclaveRequestClassCountsV1,
}

/// Enclave heap the allocator accounts for, now and at its peak.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveHeapUsageV1 {
    pub current_bytes: u64,
    pub peak_bytes: u64,
}

/// Requests the enclave dispatched, by outcome.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveRequestCountsV1 {
    pub total: u64,
    pub errored: u64,
    pub denied: u64,
}

/// Requests the enclave dispatched, by authorization class.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveRequestClassCountsV1 {
    pub initialized: u64,
    pub founding_keyless: u64,
    pub keyless_onboarding: u64,
    pub ready: u64,
    pub dev_source_seal: u64,
    pub dev_recipient_ingest: u64,
}

/// EIP-191 `personal_sign` digest of `message`. Matches ethers `signMessage`.
pub fn eip191_hash(message: &[u8]) -> B256 {
    let mut buf = Vec::with_capacity(message.len() + 40);
    buf.extend_from_slice(b"\x19Ethereum Signed Message:\n");
    push_decimal(&mut buf, message.len());
    buf.extend_from_slice(message);
    alloy_primitives::keccak256(buf)
}

/// Append `value` as ASCII decimal. Same bytes as `usize::to_string`.
fn push_decimal(buf: &mut Vec<u8>, mut value: usize) {
    let mut digits = [0u8; 20];
    let mut index = digits.len();
    loop {
        index -= 1;
        digits[index] = b'0' + u8::try_from(value % 10).unwrap_or(0);
        value /= 10;
        if value == 0 {
            break;
        }
    }
    buf.extend_from_slice(&digits[index..]);
}

/// Resident infrastructure responses. Application witness bytes stay on
/// [`crate::witness::EnclaveWitnessEntryV1::Payload`] and are not carried here.
#[derive(Clone, PartialEq, Eq)]
pub enum EnclaveResponse {
    /// Canonical accepted verdict or stable reject code.
    DcapVerificationFinishedV1 {
        request_hash: B256,
        outcome: Vec<u8>,
        attestation_tag: Vec<u8>,
    },
    /// Purpose-bound onboarding verdict plus its deterministic Registry artifact.
    DcapOnboardingVerificationFinishedV1 {
        request_hash: B256,
        outcome: Vec<u8>,
        onboarding_artifact: Vec<u8>,
        attestation_tag: Vec<u8>,
    },
    PublicKeys {
        offer_key_ready: bool,
        recipient_x25519_pub: [u8; 32],
        attestation_pub: [u8; 32],
        noise_static_pub: [u8; 32],
        tee_bls_pub: Vec<u8>,
        dkg_enc_pub: [u8; 32],
        dkg_enc_sig: Vec<u8>,
    },
    /// Generic acknowledgement (e.g. `DkgOpen` / `DkgDealerReceiveAck`).
    Ack,
    /// Seam A result.
    DkgDealt {
        pub_msg: Vec<u8>,
        sealed_shares: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// Seam B result.
    DkgPlayerAck {
        ack: Option<Vec<u8>>,
    },
    /// Seam D result.
    DkgSignedLog {
        signed_log: Vec<u8>,
    },
    /// Seam E result.
    DkgPlayerFinalized {
        group_public: Vec<u8>,
        share_commitment: B256,
    },
    /// Seam F result.
    DkgNetworkKeyPartial {
        sealed: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// Seam F founding key result.
    DkgNetworkEncryptionKey {
        network_encryption_public_key: [u8; 32],
        group_public_key: Vec<u8>,
    },
    Error {
        message: String,
    },
    /// Ceremony-scoped DKG participant identity.
    DkgParticipantAnnounceV1 {
        participant: ParticipantAnnounce,
    },
    /// Signed DirectDev onboarding artifact prepared for one canonical request.
    NoAttestationDevOnboardingArtifactPreparedV2 {
        request_hash: B256,
        onboarding_artifact: Vec<u8>,
        attestation_tag: Vec<u8>,
    },
}

impl core::fmt::Debug for EnclaveResponse {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Error { message } => f.debug_struct("Error").field("message", message).finish(),
            _ => f.debug_struct("EnclaveResponse").finish_non_exhaustive(),
        }
    }
}
