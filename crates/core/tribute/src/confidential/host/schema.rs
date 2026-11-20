//! Witness bodies for the frozen Tribute tags 7 and 11–14 and 16–18.

use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};
use outbe_core_common::confidential_tee::WitnessResponseTag;
use outbe_core_common::confidential_tee::WitnessSchema;
use outbe_core_common::nod_encryption::EncryptedNodV1;
use outbe_core_common::tribute_day_encryption::EncryptedTributeDayAmountV2;
use outbe_core_common::tribute_encryption::TributeAmountsV2;
use serde::{Deserialize, Serialize};

use super::super::day_protocol::LysisFractionsResultV1;
use super::super::nod_bridge::IssueNodFromTributeOutcomeV1;
use super::super::protocol::TributeOfferResult;
use super::super::tribute_v2::EncryptedTributeOfferResultV2;

macro_rules! tribute_schema {
    ($ty:ty, $tag:expr) => {
        impl WitnessSchema for $ty {
            const TAG: WitnessResponseTag = $tag;

            fn inputs_canonical_hash(&self) -> B256 {
                self.inputs_canonical_hash
            }

            fn attestation_tag(&self) -> &[u8] {
                &self.attestation_tag
            }

            fn set_attestation_tag(&mut self, tag: Vec<u8>) {
                self.attestation_tag = tag;
            }
        }
    };
}

/// Tag 11 body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedTributeOfferBatchV2 {
    /// Per-offer results.
    pub results: Vec<EncryptedTributeOfferResultV2>,
    /// Canonical commitment over the offers and chain.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// Tag 12 body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TributeAmountsReadV2 {
    /// Decrypted amounts, one per input record.
    pub amounts: Vec<TributeAmountsV2>,
    /// Canonical commitment over the encrypted records.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// Tag 13 body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TributeDayOpAppliedV2 {
    /// Updated encrypted day record.
    pub record: EncryptedTributeDayAmountV2,
    /// Canonical commitment over the operation request.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// Tag 14 body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TributeDayAmountReadV2 {
    /// Decrypted day nominal.
    pub amount: U256,
    /// Canonical commitment over the encrypted record.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// Tag 16 body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LysisFractionsExecutedV1 {
    /// Confidential Lysis result.
    pub result: LysisFractionsResultV1,
    /// Canonical commitment over the request.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// Tag 17 body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TributeDayFractionsReadV1 {
    /// Decrypted per-league fractions.
    pub fractions: BTreeMap<u16, u64>,
    /// Canonical commitment over the encrypted record.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// Tag 7 body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodEncryptedForTributeV1 {
    /// Encrypted NOD bound to the Tribute.
    pub nod: EncryptedNodV1,
    /// Canonical commitment over the create inputs.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// Tag 18 body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodIssuedFromTributeV1 {
    /// Issuance outcome.
    pub outcome: IssueNodFromTributeOutcomeV1,
    /// Canonical commitment over the request.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// Session-only tag 0 body. It is not a witness schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TributeOfferBatch {
    /// Per-offer public results.
    pub results: Vec<TributeOfferResult>,
    /// Canonical commitment over the offers.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature.
    pub attestation_tag: Vec<u8>,
}

tribute_schema!(
    EncryptedTributeOfferBatchV2,
    WitnessResponseTag::EncryptedTributeOfferBatchV2
);
tribute_schema!(
    TributeAmountsReadV2,
    WitnessResponseTag::TributeAmountsReadV2
);
tribute_schema!(
    TributeDayOpAppliedV2,
    WitnessResponseTag::TributeDayOpAppliedV2
);
tribute_schema!(
    TributeDayAmountReadV2,
    WitnessResponseTag::TributeDayAmountReadV2
);
tribute_schema!(
    LysisFractionsExecutedV1,
    WitnessResponseTag::LysisFractionsExecutedV1
);
tribute_schema!(
    TributeDayFractionsReadV1,
    WitnessResponseTag::TributeDayFractionsReadV1
);
tribute_schema!(
    NodEncryptedForTributeV1,
    WitnessResponseTag::NodEncryptedForTributeV1
);
tribute_schema!(
    NodIssuedFromTributeV1,
    WitnessResponseTag::NodIssuedFromTributeV1
);
