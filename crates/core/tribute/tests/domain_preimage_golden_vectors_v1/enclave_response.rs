//! Serde mirror of the retired domain `EnclaveResponse` enum.
//! Postcard indexes follow declaration order. Nested bodies are the public types.
use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, U256};
use outbe_core_common::nod_encryption::{EncryptedNodGroupLoadV1, EncryptedNodV1};
use outbe_core_common::tribute_day_encryption::EncryptedTributeDayAmountV2;
use outbe_core_common::tribute_encryption::TributeAmountsV2;
use outbe_fidelity::{FidelityCohortResult, FidelityLeagueEntry, FidelityQueryResult};
use outbe_gratis::nod_mine::MineEncryptedNodResultV1;
use outbe_gratis::GratisOpResult;
use outbe_promis::PromisOpResult;
use outbe_tribute::confidential::{
    EncryptedTributeOfferResultV2, IssueNodFromTributeOutcomeV1, LysisFractionsResultV1,
    TributeOfferResult,
};

/// Historical domain response. The witness wire is a different encoding.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnclaveResponse {
    TributeOfferBatch {
        results: Vec<TributeOfferResult>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    GratisOpApplied {
        result: Box<GratisOpResult>,
    },
    PromisOpApplied {
        result: Box<PromisOpResult>,
    },
    FidelityCohortApplied {
        result: Box<FidelityCohortResult>,
    },
    FidelityLeaguesSnapshotted {
        leagues: Vec<FidelityLeagueEntry>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    FidelityIndexQueried {
        result: Box<FidelityQueryResult>,
    },
    AccountKeysSealed {
        account: Address,
        sealed: Vec<u8>,
        nonce: [u8; 12],
        enclave_ephemeral_pubkey: [u8; 32],
    },
    NodEncryptedForTributeV1 {
        nod: EncryptedNodV1,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    EncryptedNodMintRejectedV1 {
        reason: String,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    EncryptedNodMinedV1 {
        result: Box<MineEncryptedNodResultV1>,
    },
    NodAmountReadV1 {
        amount: U256,
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
    FidelityQueryRejected {
        inputs_canonical_hash: B256,
        reason: String,
        attestation_tag: Vec<u8>,
    },
    LysisFractionsExecutedV1 {
        result: Box<LysisFractionsResultV1>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    TributeDayFractionsReadV1 {
        fractions: BTreeMap<u16, u64>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodIssuedFromTributeV1 {
        outcome: IssueNodFromTributeOutcomeV1,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodGroupLoadOpAppliedV1 {
        record: EncryptedNodGroupLoadV1,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodGroupLoadReadV1 {
        amount: U256,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodQuoteEncryptedV1 {
        encrypted_payment: Vec<u8>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
}
