//! The persisted V1 schema stays flat while runtime checkpoints share material.

use super::*;

mod decode;
mod encode;

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename = "UpgradeJournalStateV1"
)]
pub(super) enum UpgradeJournalWire<C, S, T> {
    CandidatePrepared(PreparedWire<C>),
    #[serde(alias = "rootCopied")]
    KeyProvisioned(ProvisionedWire<C>),
    CandidateKeyReady(ReadyWire<C>),
    SubmissionPrepared(SubmissionWire<C, S>),
    Submitted(SubmittedWire<C, S, T>),
    Finalized(CompletedWire<C, S>),
    Promoted(CompletedWire<C, S>),
    TerminalMissedCutoff(TerminalWire<C>),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreparedWire<C> {
    context: C,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProvisionedWire<C> {
    context: C,
    sealed_root_hash: B256,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadyWire<C> {
    context: C,
    sealed_root_hash: B256,
    resident_offer_public: B256,
    proof_hash: B256,
}

impl<C> ReadyWire<C> {
    fn security(&self) -> UpgradeSecurityMaterialV1 {
        UpgradeSecurityMaterialV1 {
            sealed_root_hash: self.sealed_root_hash,
            resident_offer_public: self.resident_offer_public,
            proof_hash: self.proof_hash,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SubmissionWire<C, S> {
    context: C,
    sealed_root_hash: B256,
    resident_offer_public: B256,
    proof_hash: B256,
    submission: S,
}

impl<C, S> SubmissionWire<C, S> {
    fn security(&self) -> UpgradeSecurityMaterialV1 {
        UpgradeSecurityMaterialV1 {
            sealed_root_hash: self.sealed_root_hash,
            resident_offer_public: self.resident_offer_public,
            proof_hash: self.proof_hash,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SubmittedWire<C, S, T> {
    context: C,
    sealed_root_hash: B256,
    resident_offer_public: B256,
    proof_hash: B256,
    submission: S,
    submitted_at_finalized_height: u64,
    transaction_hashes: T,
}

impl<C, S, T> SubmittedWire<C, S, T> {
    fn security(&self) -> UpgradeSecurityMaterialV1 {
        UpgradeSecurityMaterialV1 {
            sealed_root_hash: self.sealed_root_hash,
            resident_offer_public: self.resident_offer_public,
            proof_hash: self.proof_hash,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CompletedWire<C, S> {
    context: C,
    sealed_root_hash: B256,
    resident_offer_public: B256,
    proof_hash: B256,
    submission: S,
    finalized_height: u64,
    finalized_hash: B256,
}

impl<C, S> CompletedWire<C, S> {
    fn security(&self) -> UpgradeSecurityMaterialV1 {
        UpgradeSecurityMaterialV1 {
            sealed_root_hash: self.sealed_root_hash,
            resident_offer_public: self.resident_offer_public,
            proof_hash: self.proof_hash,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TerminalWire<C> {
    context: C,
    finalized_height: u64,
    activation_height: u64,
}

type OwnedWire = UpgradeJournalWire<UpgradeContextV1, PreparedUpgradeSubmissionV1, Vec<B256>>;
type BorrowedWire<'a> =
    UpgradeJournalWire<&'a UpgradeContextV1, &'a PreparedUpgradeSubmissionV1, &'a Vec<B256>>;

impl Serialize for UpgradeJournalStateV1 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        encode::borrowed(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for UpgradeJournalStateV1 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        OwnedWire::deserialize(deserializer).map(decode::owned)
    }
}
