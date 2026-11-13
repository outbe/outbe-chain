use alloy_primitives::B256;
use serde::{Deserialize, Serialize};

use crate::protocol::EnclaveResponse;

/// Stable witness-only response tags. Variant order is frozen independently of [`EnclaveResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnclaveWitnessEntryV1 {
    DcapVerificationFinishedV1 {
        request_hash: B256,
        outcome: Vec<u8>,
        attestation_tag: Vec<u8>,
    },
    DcapOnboardingVerificationFinishedV1 {
        request_hash: B256,
        outcome: Vec<u8>,
        onboarding_artifact: Vec<u8>,
        attestation_tag: Vec<u8>,
    },
    GramineDirectDevOnboardingArtifactPreparedV2 {
        request_hash: B256,
        onboarding_artifact: Vec<u8>,
        attestation_tag: Vec<u8>,
    },
    /// Opaque tag-free application-layer witness payload with its canonical input
    /// commitment and local-only attestation signature.
    Payload {
        payload: Vec<u8>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
}

impl EnclaveWitnessEntryV1 {
    /// Compares every field except the local-only attestation-tag fields.
    pub fn semantic_eq_ignoring_attestation_tag(&self, other: &Self) -> bool {
        use EnclaveWitnessEntryV1::*;
        match (self, other) {
            (
                DcapVerificationFinishedV1 {
                    request_hash: a,
                    outcome: ao,
                    ..
                },
                DcapVerificationFinishedV1 {
                    request_hash: b,
                    outcome: bo,
                    ..
                },
            ) => a == b && ao == bo,
            (
                DcapOnboardingVerificationFinishedV1 {
                    request_hash: a,
                    outcome: ao,
                    onboarding_artifact: aa,
                    ..
                },
                DcapOnboardingVerificationFinishedV1 {
                    request_hash: b,
                    outcome: bo,
                    onboarding_artifact: ba,
                    ..
                },
            ) => a == b && ao == bo && aa == ba,
            (
                GramineDirectDevOnboardingArtifactPreparedV2 {
                    request_hash: a,
                    onboarding_artifact: aa,
                    ..
                },
                GramineDirectDevOnboardingArtifactPreparedV2 {
                    request_hash: b,
                    onboarding_artifact: ba,
                    ..
                },
            ) => a == b && aa == ba,
            (
                Payload {
                    payload: ap,
                    inputs_canonical_hash: ah,
                    ..
                },
                Payload {
                    payload: bp,
                    inputs_canonical_hash: bh,
                    ..
                },
            ) => ap == bp && ah == bh,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnclaveWitnessTapeV1 {
    pub prefix: Vec<EnclaveWitnessEntryV1>,
    pub txs: Vec<TxWitnessV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxWitnessV1 {
    pub tx_index: u32,
    pub entries: Vec<EnclaveWitnessEntryV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedEnclaveResponse;

impl TryFrom<EnclaveResponse> for EnclaveWitnessEntryV1 {
    type Error = UnsupportedEnclaveResponse;

    fn try_from(value: EnclaveResponse) -> Result<Self, Self::Error> {
        use EnclaveResponse as R;
        Ok(match value {
            R::DcapVerificationFinishedV1 {
                request_hash,
                outcome,
                attestation_tag,
            } => Self::DcapVerificationFinishedV1 {
                request_hash,
                outcome,
                attestation_tag,
            },
            R::DcapOnboardingVerificationFinishedV1 {
                request_hash,
                outcome,
                onboarding_artifact,
                attestation_tag,
            } => Self::DcapOnboardingVerificationFinishedV1 {
                request_hash,
                outcome,
                onboarding_artifact,
                attestation_tag,
            },
            R::GramineDirectDevOnboardingArtifactPreparedV2 {
                request_hash,
                onboarding_artifact,
                attestation_tag,
            } => Self::GramineDirectDevOnboardingArtifactPreparedV2 {
                request_hash,
                onboarding_artifact,
                attestation_tag,
            },
            R::Payload {
                recordable: true,
                payload,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::Payload {
                payload,
                inputs_canonical_hash,
                attestation_tag,
            },
            _ => return Err(UnsupportedEnclaveResponse),
        })
    }
}

impl From<EnclaveWitnessEntryV1> for EnclaveResponse {
    fn from(value: EnclaveWitnessEntryV1) -> Self {
        use EnclaveWitnessEntryV1 as W;
        match value {
            W::DcapVerificationFinishedV1 {
                request_hash,
                outcome,
                attestation_tag,
            } => Self::DcapVerificationFinishedV1 {
                request_hash,
                outcome,
                attestation_tag,
            },
            W::DcapOnboardingVerificationFinishedV1 {
                request_hash,
                outcome,
                onboarding_artifact,
                attestation_tag,
            } => Self::DcapOnboardingVerificationFinishedV1 {
                request_hash,
                outcome,
                onboarding_artifact,
                attestation_tag,
            },
            W::GramineDirectDevOnboardingArtifactPreparedV2 {
                request_hash,
                onboarding_artifact,
                attestation_tag,
            } => Self::GramineDirectDevOnboardingArtifactPreparedV2 {
                request_hash,
                onboarding_artifact,
                attestation_tag,
            },
            W::Payload {
                payload,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::Payload {
                recordable: true,
                payload,
                inputs_canonical_hash,
                attestation_tag,
            },
        }
    }
}
