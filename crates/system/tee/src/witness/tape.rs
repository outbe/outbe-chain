use alloy_primitives::{B256, U256};
use serde::{Deserialize, Serialize};

use crate::protocol::{
    EnclaveResponse, FidelityLeagueEntry, FidelityQueryResult, GratisOpResult, PromisOpResult,
};
use crate::{nod_mine::MineEncryptedNodResultV1, tribute_v2::EncryptedTributeOfferResultV2};
use outbe_primitives::{
    nod_encryption::EncryptedNodV1, tribute_day_encryption::EncryptedTributeDayAmountV2,
    tribute_encryption::TributeAmountsV2,
};

/// Stable witness-only response tags. Variant order is frozen independently of EnclaveResponse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnclaveWitnessEntryV1 {
    GratisOpApplied {
        result: Box<GratisOpResult>,
    },
    PromisOpApplied {
        result: Box<PromisOpResult>,
    },
    EncryptedNodMinedV1 {
        result: Box<MineEncryptedNodResultV1>,
    },
    EncryptedNodMintRejectedV1 {
        reason: String,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodAmountReadV1 {
        amount: U256,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodEncryptedForTributeV1 {
        nod: EncryptedNodV1,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    EncryptedTributeOfferBatchV2 {
        results: Vec<EncryptedTributeOfferResultV2>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    TributeAmountsReadV2 {
        amounts: Vec<TributeAmountsV2>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    TributeDayOpAppliedV2 {
        record: EncryptedTributeDayAmountV2,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    TributeDayAmountReadV2 {
        amount: U256,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    FidelityLeaguesSnapshotted {
        leagues: Vec<FidelityLeagueEntry>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    FidelityIndexQueried {
        result: Box<FidelityQueryResult>,
    },
    FidelityQueryRejected {
        reason: String,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
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
}

impl EnclaveWitnessEntryV1 {
    /// Compares every field except the named local-only attestation-tag fields.
    pub fn semantic_eq_ignoring_attestation_tag(&self, other: &Self) -> bool {
        use EnclaveWitnessEntryV1::*;
        match (self, other) {
            (GratisOpApplied { result: a }, GratisOpApplied { result: b }) => {
                let mut a = (**a).clone();
                let mut b = (**b).clone();
                a.attestation_tag.clear();
                b.attestation_tag.clear();
                a == b
            }
            (PromisOpApplied { result: a }, PromisOpApplied { result: b }) => {
                let mut a = (**a).clone();
                let mut b = (**b).clone();
                a.attestation_tag.clear();
                b.attestation_tag.clear();
                a == b
            }
            (EncryptedNodMinedV1 { result: a }, EncryptedNodMinedV1 { result: b }) => {
                let mut a = (**a).clone();
                let mut b = (**b).clone();
                a.attestation_tag.clear();
                b.attestation_tag.clear();
                a == b
            }
            (
                EncryptedNodMintRejectedV1 {
                    reason: a,
                    inputs_canonical_hash: ah,
                    ..
                },
                EncryptedNodMintRejectedV1 {
                    reason: b,
                    inputs_canonical_hash: bh,
                    ..
                },
            ) => a == b && ah == bh,
            (
                NodAmountReadV1 {
                    amount: a,
                    inputs_canonical_hash: ah,
                    ..
                },
                NodAmountReadV1 {
                    amount: b,
                    inputs_canonical_hash: bh,
                    ..
                },
            ) => a == b && ah == bh,
            (
                NodEncryptedForTributeV1 {
                    nod: a,
                    inputs_canonical_hash: ah,
                    ..
                },
                NodEncryptedForTributeV1 {
                    nod: b,
                    inputs_canonical_hash: bh,
                    ..
                },
            ) => a == b && ah == bh,
            (
                EncryptedTributeOfferBatchV2 {
                    results: a,
                    inputs_canonical_hash: ah,
                    ..
                },
                EncryptedTributeOfferBatchV2 {
                    results: b,
                    inputs_canonical_hash: bh,
                    ..
                },
            ) => a == b && ah == bh,
            (
                TributeAmountsReadV2 {
                    amounts: a,
                    inputs_canonical_hash: ah,
                    ..
                },
                TributeAmountsReadV2 {
                    amounts: b,
                    inputs_canonical_hash: bh,
                    ..
                },
            ) => a == b && ah == bh,
            (
                TributeDayOpAppliedV2 {
                    record: a,
                    inputs_canonical_hash: ah,
                    ..
                },
                TributeDayOpAppliedV2 {
                    record: b,
                    inputs_canonical_hash: bh,
                    ..
                },
            ) => a == b && ah == bh,
            (
                TributeDayAmountReadV2 {
                    amount: a,
                    inputs_canonical_hash: ah,
                    ..
                },
                TributeDayAmountReadV2 {
                    amount: b,
                    inputs_canonical_hash: bh,
                    ..
                },
            ) => a == b && ah == bh,
            (
                FidelityLeaguesSnapshotted {
                    leagues: a,
                    inputs_canonical_hash: ah,
                    ..
                },
                FidelityLeaguesSnapshotted {
                    leagues: b,
                    inputs_canonical_hash: bh,
                    ..
                },
            ) => a == b && ah == bh,
            (FidelityIndexQueried { result: a }, FidelityIndexQueried { result: b }) => {
                let mut a = (**a).clone();
                let mut b = (**b).clone();
                a.attestation_tag.clear();
                b.attestation_tag.clear();
                a == b
            }
            (
                FidelityQueryRejected {
                    reason: a,
                    inputs_canonical_hash: ah,
                    ..
                },
                FidelityQueryRejected {
                    reason: b,
                    inputs_canonical_hash: bh,
                    ..
                },
            ) => a == b && ah == bh,
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
            R::GratisOpApplied { result } => Self::GratisOpApplied { result },
            R::PromisOpApplied { result } => Self::PromisOpApplied { result },
            R::EncryptedNodMinedV1 { result } => Self::EncryptedNodMinedV1 { result },
            R::EncryptedNodMintRejectedV1 {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::EncryptedNodMintRejectedV1 {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            },
            R::NodAmountReadV1 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::NodAmountReadV1 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            },
            R::NodEncryptedForTributeV1 {
                nod,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::NodEncryptedForTributeV1 {
                nod,
                inputs_canonical_hash,
                attestation_tag,
            },
            R::EncryptedTributeOfferBatchV2 {
                results,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::EncryptedTributeOfferBatchV2 {
                results,
                inputs_canonical_hash,
                attestation_tag,
            },
            R::TributeAmountsReadV2 {
                amounts,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::TributeAmountsReadV2 {
                amounts,
                inputs_canonical_hash,
                attestation_tag,
            },
            R::TributeDayOpAppliedV2 {
                record,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::TributeDayOpAppliedV2 {
                record,
                inputs_canonical_hash,
                attestation_tag,
            },
            R::TributeDayAmountReadV2 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::TributeDayAmountReadV2 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            },
            R::FidelityLeaguesSnapshotted {
                leagues,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::FidelityLeaguesSnapshotted {
                leagues,
                inputs_canonical_hash,
                attestation_tag,
            },
            R::FidelityIndexQueried { result } => Self::FidelityIndexQueried { result },
            R::FidelityQueryRejected {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::FidelityQueryRejected {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            },
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
            _ => return Err(UnsupportedEnclaveResponse),
        })
    }
}

impl From<EnclaveWitnessEntryV1> for EnclaveResponse {
    fn from(value: EnclaveWitnessEntryV1) -> Self {
        use EnclaveWitnessEntryV1 as W;
        match value {
            W::GratisOpApplied { result } => Self::GratisOpApplied { result },
            W::PromisOpApplied { result } => Self::PromisOpApplied { result },
            W::EncryptedNodMinedV1 { result } => Self::EncryptedNodMinedV1 { result },
            W::EncryptedNodMintRejectedV1 {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::EncryptedNodMintRejectedV1 {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            },
            W::NodAmountReadV1 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::NodAmountReadV1 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            },
            W::NodEncryptedForTributeV1 {
                nod,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::NodEncryptedForTributeV1 {
                nod,
                inputs_canonical_hash,
                attestation_tag,
            },
            W::EncryptedTributeOfferBatchV2 {
                results,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::EncryptedTributeOfferBatchV2 {
                results,
                inputs_canonical_hash,
                attestation_tag,
            },
            W::TributeAmountsReadV2 {
                amounts,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::TributeAmountsReadV2 {
                amounts,
                inputs_canonical_hash,
                attestation_tag,
            },
            W::TributeDayOpAppliedV2 {
                record,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::TributeDayOpAppliedV2 {
                record,
                inputs_canonical_hash,
                attestation_tag,
            },
            W::TributeDayAmountReadV2 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::TributeDayAmountReadV2 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            },
            W::FidelityLeaguesSnapshotted {
                leagues,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::FidelityLeaguesSnapshotted {
                leagues,
                inputs_canonical_hash,
                attestation_tag,
            },
            W::FidelityIndexQueried { result } => Self::FidelityIndexQueried { result },
            W::FidelityQueryRejected {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::FidelityQueryRejected {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            },
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
        }
    }
}
