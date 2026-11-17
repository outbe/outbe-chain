//! Owner-only, crash-consistent enclave upgrade checkpoints (`tee-upgrade-v1`).
//!
//! Shared between the node promotion/monitor workers and operator CLI commands
//! without any HTTP/RPC client dependencies.

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read as _, Write as _},
    os::unix::fs::{
        DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    },
    path::{Path, PathBuf},
};

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolCall as _;
use outbe_primitives::{
    tee_attestation_v1::{AttestationEvidenceV1, RegistrationIntentV1, TransitionKeyReadyProofV1},
    tee_registry_abi_v1::ITeeRegistryV1,
};
use serde::{Deserialize, Serialize};

use crate::journal_storage::{sync_directory, JournalPaths, JournalSnapshotV1};

mod context;
mod errors;
mod journal_validation;
mod storage;
mod wire;

#[cfg(test)]
mod compatibility_tests;
#[cfg(test)]
mod tests;

pub use errors::UpgradeError;
use journal_validation::validate_checkpoint_transition;
pub use journal_validation::{security_material, SecurityMaterial};
use storage::{
    read_private_bounded_file, read_snapshot, validate_directory, validate_private_file,
};

const DIRECTORY: &str = "tee-upgrade-v1";
const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RELAY_VARIANTS: usize = 8;

/// Signed relay transaction record persisted in lifecycle journals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RawRelayTransactionV1 {
    pub relay: Address,
    pub chain_id: u64,
    pub account_nonce: u64,
    pub gas_price: U256,
    pub gas_limit: u64,
    pub calldata_hash: B256,
    pub raw_transaction: Vec<u8>,
    pub transaction_hash: B256,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpgradeContextV1 {
    pub predecessor_manifest_hash: B256,
    pub candidate_manifest_hash: B256,
    pub successor_policy_hash: B256,
    pub activation_height: u64,
    pub active_tee_dir: PathBuf,
    pub candidate_tee_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedUpgradeSubmissionV1 {
    pub intent_hash: B256,
    pub evidence_hash: B256,
    pub calldata_hash: B256,
    pub relay: Address,
    pub relay_variants: Vec<RawRelayTransactionV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpgradeSubmissionOutcomeV1 {
    Submitted {
        transaction_hash: B256,
        replayed: bool,
    },
    AlreadySubmitted {
        transaction_hash: B256,
    },
    Finalized {
        transaction_hash: B256,
        finalized_height: u64,
    },
    Promoted {
        transaction_hash: B256,
        finalized_height: u64,
    },
}

/// Commitments established when the candidate key becomes ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpgradeSecurityMaterialV1 {
    pub sealed_root_hash: B256,
    pub resident_offer_public: B256,
    pub proof_hash: B256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpgradeJournalStateV1 {
    CandidatePrepared {
        context: UpgradeContextV1,
    },
    KeyProvisioned {
        context: UpgradeContextV1,
        sealed_root_hash: B256,
    },
    CandidateKeyReady {
        context: UpgradeContextV1,
        security: UpgradeSecurityMaterialV1,
    },
    SubmissionPrepared {
        context: UpgradeContextV1,
        security: UpgradeSecurityMaterialV1,
        submission: PreparedUpgradeSubmissionV1,
    },
    Submitted {
        context: UpgradeContextV1,
        security: UpgradeSecurityMaterialV1,
        submission: PreparedUpgradeSubmissionV1,
        submitted_at_finalized_height: u64,
        transaction_hashes: Vec<B256>,
    },
    Finalized {
        context: UpgradeContextV1,
        security: UpgradeSecurityMaterialV1,
        submission: PreparedUpgradeSubmissionV1,
        finalized_height: u64,
        finalized_hash: B256,
    },
    Promoted {
        context: UpgradeContextV1,
        security: UpgradeSecurityMaterialV1,
        submission: PreparedUpgradeSubmissionV1,
        finalized_height: u64,
        finalized_hash: B256,
    },
    TerminalMissedCutoff {
        context: UpgradeContextV1,
        finalized_height: u64,
        activation_height: u64,
    },
}

impl UpgradeJournalStateV1 {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::CandidatePrepared { .. } => "candidatePrepared",
            Self::KeyProvisioned { .. } => "keyProvisioned",
            Self::CandidateKeyReady { .. } => "candidateKeyReady",
            Self::SubmissionPrepared { .. } => "submissionPrepared",
            Self::Submitted { .. } => "submitted",
            Self::Finalized { .. } => "finalized",
            Self::Promoted { .. } => "promoted",
            Self::TerminalMissedCutoff { .. } => "terminalMissedCutoff",
        }
    }

    #[must_use]
    pub const fn context(&self) -> &UpgradeContextV1 {
        match self {
            Self::CandidatePrepared { context }
            | Self::KeyProvisioned { context, .. }
            | Self::CandidateKeyReady { context, .. }
            | Self::SubmissionPrepared { context, .. }
            | Self::Submitted { context, .. }
            | Self::Finalized { context, .. }
            | Self::Promoted { context, .. }
            | Self::TerminalMissedCutoff { context, .. } => context,
        }
    }
}

/// V1 upgrade journal envelope, retaining the upgrade lifecycle type.
pub type UpgradeJournalSnapshotV1 = JournalSnapshotV1<UpgradeJournalStateV1>;

impl UpgradeJournalSnapshotV1 {
    pub fn validate(&self) -> Result<(), UpgradeError> {
        if self.version != 1 || self.generation == 0 {
            return Err(UpgradeError::UnsupportedVersionOrGeneration);
        }
        self.lifecycle.validate()
    }
}

pub struct UpgradeJournalGuardV1 {
    paths: JournalPaths,
    _lock: File,
}

fn is_next_upgrade(current: &UpgradeJournalStateV1, next: &UpgradeJournalStateV1) -> bool {
    let (
        UpgradeJournalStateV1::Promoted { context: old, .. },
        UpgradeJournalStateV1::CandidatePrepared { context: new },
    ) = (current, next)
    else {
        return false;
    };
    new.predecessor_manifest_hash == old.candidate_manifest_hash
        && new.activation_height > old.activation_height
        && new.successor_policy_hash != old.successor_policy_hash
}

pub fn inspect_upgrade_journal_v1(
    node_data_dir: &Path,
) -> Result<Option<UpgradeJournalSnapshotV1>, UpgradeError> {
    let paths = JournalPaths::new(node_data_dir, DIRECTORY);
    if !paths.root.exists() {
        return Ok(None);
    }
    validate_directory(&paths.root)?;
    read_snapshot(&paths.journal)
}

pub fn prepare_upgrade_journal_with_check_v1<E>(
    node_data_dir: &Path,
    context: UpgradeContextV1,
    before_store: impl FnOnce() -> Result<(), E>,
) -> Result<UpgradeJournalSnapshotV1, E>
where
    E: From<UpgradeError>,
{
    context.validate()?;
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    if let Some(existing) = guard.load()? {
        if existing.lifecycle.context() == &context {
            return Ok(existing);
        }
        if !is_next_upgrade(
            &existing.lifecycle,
            &UpgradeJournalStateV1::CandidatePrepared {
                context: context.clone(),
            },
        ) {
            return Err(UpgradeError::UpgradeAlreadyJournaled.into());
        }
    }
    before_store()?;
    let snapshot =
        UpgradeJournalSnapshotV1::new(UpgradeJournalStateV1::CandidatePrepared { context });
    guard.store(snapshot)?;
    guard
        .load()?
        .ok_or_else(|| UpgradeError::CheckpointDisappeared("prepared upgrade journal").into())
}

pub fn prepare_upgrade_journal_unchecked_v1(
    node_data_dir: &Path,
    context: UpgradeContextV1,
) -> Result<UpgradeJournalSnapshotV1, UpgradeError> {
    prepare_upgrade_journal_with_check_v1(node_data_dir, context, || Ok(()))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkUpgradeSubmissionV1 {
    pub candidate_manifest_hash: B256,
    pub evidence: Vec<u8>,
    pub context: Vec<u8>,
    pub calldata: Vec<u8>,
    pub transaction: RawRelayTransactionV1,
}

impl UpgradeJournalGuardV1 {
    pub fn load_network_submission(
        &self,
    ) -> Result<Option<NetworkUpgradeSubmissionV1>, UpgradeError> {
        let path = self.paths.root.join("network-submission.json");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = read_private_bounded_file(&path, MAX_JOURNAL_BYTES)?;
        let value: NetworkUpgradeSubmissionV1 =
            serde_json::from_slice(&bytes).map_err(|source| UpgradeError::Json {
                context: "decode upgrade network submission",
                source,
            })?;
        value.validate()?;
        Ok(Some(value))
    }

    pub fn store_network_submission(
        &self,
        value: &NetworkUpgradeSubmissionV1,
    ) -> Result<(), UpgradeError> {
        value.validate()?;
        let encoded = serde_json::to_vec(value).map_err(|source| UpgradeError::Json {
            context: "encode upgrade network submission",
            source,
        })?;
        if encoded.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(UpgradeError::NetworkSubmissionExceedsCap);
        }
        let next = self.paths.root.join("network-submission.next");
        if next.exists() {
            validate_private_file(&next, MAX_JOURNAL_BYTES)?;
            fs::remove_file(&next).map_err(|source| UpgradeError::Io {
                context: "remove stale network submission scratch",
                source,
            })?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&next)
            .map_err(|source| UpgradeError::Io {
                context: "create network submission scratch",
                source,
            })?;
        file.write_all(&encoded)
            .map_err(|source| UpgradeError::Io {
                context: "write network submission scratch",
                source,
            })?;
        file.sync_all().map_err(|source| UpgradeError::Io {
            context: "fsync network submission scratch",
            source,
        })?;
        fs::rename(next, self.paths.root.join("network-submission.json")).map_err(|source| {
            UpgradeError::Io {
                context: "commit network submission",
                source,
            }
        })?;
        sync_directory(&self.paths.root)?;
        Ok(())
    }
}

pub fn record_candidate_key_ready_v1(
    node_data_dir: &Path,
    intent: &RegistrationIntentV1,
    proof: &TransitionKeyReadyProofV1,
    expected_offer_public: [u8; 32],
) -> Result<UpgradeJournalSnapshotV1, UpgradeError> {
    proof
        .verify_for_transition(intent, expected_offer_public)
        .map_err(UpgradeError::InvalidKeyReadyProof)?;
    let proof_hash = keccak256(
        proof
            .encode_canonical()
            .map_err(UpgradeError::EncodeKeyReadyProof)?,
    );
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::CandidateNotPrepared)?;
    let UpgradeJournalStateV1::KeyProvisioned {
        context,
        sealed_root_hash,
    } = current.lifecycle
    else {
        return Err(UpgradeError::KeyReadyRequiresKeyProvisioned);
    };
    if proof.candidate_manifest_hash != context.candidate_manifest_hash {
        return Err(UpgradeError::KeyReadyProofManifestMismatch);
    }
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::CandidateKeyReady {
            context,
            security: UpgradeSecurityMaterialV1 {
                sealed_root_hash,
                resident_offer_public: B256::from(expected_offer_public),
                proof_hash,
            },
        },
    ))?;
    guard.load()?.ok_or(UpgradeError::CheckpointDisappeared(
        "candidate-key-ready checkpoint",
    ))
}

pub fn record_upgrade_submission_prepared_v1(
    node_data_dir: &Path,
    submission: PreparedUpgradeSubmissionV1,
) -> Result<UpgradeJournalSnapshotV1, UpgradeError> {
    submission.validate()?;
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::CandidateNotPrepared)?;
    let UpgradeJournalStateV1::CandidateKeyReady { context, security } = current.lifecycle else {
        return Err(UpgradeError::SubmissionRequiresKeyReady);
    };
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::SubmissionPrepared {
            context,
            security,
            submission,
        },
    ))?;
    guard.load()?.ok_or(UpgradeError::CheckpointDisappeared(
        "submission-prepared checkpoint",
    ))
}

pub fn record_upgrade_submitted_v1(
    node_data_dir: &Path,
    finalized_height: u64,
) -> Result<UpgradeJournalSnapshotV1, UpgradeError> {
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::CandidateNotPrepared)?;
    let UpgradeJournalStateV1::SubmissionPrepared {
        context,
        security,
        submission,
    } = current.lifecycle
    else {
        return Err(UpgradeError::RelayRequiresSubmissionPrepared);
    };
    let transaction_hashes = submission
        .relay_variants
        .iter()
        .map(|variant| variant.transaction_hash)
        .collect();
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::Submitted {
            context,
            security,
            submission,
            submitted_at_finalized_height: finalized_height,
            transaction_hashes,
        },
    ))?;
    guard.load()?.ok_or(UpgradeError::CheckpointDisappeared(
        "submitted upgrade checkpoint",
    ))
}

pub fn record_upgrade_finalized_v1(
    node_data_dir: &Path,
    finalized_height: u64,
    finalized_hash: B256,
) -> Result<UpgradeJournalSnapshotV1, UpgradeError> {
    if finalized_height == 0 || finalized_hash.is_zero() {
        return Err(UpgradeError::IncompleteFinalizedAuthority);
    }
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::CandidateNotPrepared)?;
    let UpgradeJournalStateV1::Submitted {
        context,
        security,
        submission,
        ..
    } = current.lifecycle
    else {
        return Err(UpgradeError::FinalizationRequiresSubmitted);
    };
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::Finalized {
            context,
            security,
            submission,
            finalized_height,
            finalized_hash,
        },
    ))?;
    guard.load()?.ok_or(UpgradeError::CheckpointDisappeared(
        "finalized upgrade checkpoint",
    ))
}

pub fn record_upgrade_promoted_v1(
    node_data_dir: &Path,
) -> Result<UpgradeJournalSnapshotV1, UpgradeError> {
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::CandidateNotPrepared)?;
    let UpgradeJournalStateV1::Finalized {
        context,
        security,
        submission,
        finalized_height,
        finalized_hash,
    } = current.lifecycle
    else {
        return Err(UpgradeError::PromotionRequiresFinalized);
    };
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::Promoted {
            context,
            security,
            submission,
            finalized_height,
            finalized_hash,
        },
    ))?;
    guard.load()?.ok_or(UpgradeError::CheckpointDisappeared(
        "promoted upgrade checkpoint",
    ))
}

pub fn record_upgrade_missed_cutoff_v1(
    node_data_dir: &Path,
    finalized_height: u64,
    activation_height: u64,
) -> Result<UpgradeJournalSnapshotV1, UpgradeError> {
    if activation_height == 0 || finalized_height < activation_height {
        return Err(UpgradeError::CutoffNotReached);
    }
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::CandidateNotPrepared)?;
    if matches!(
        current.lifecycle,
        UpgradeJournalStateV1::Finalized { .. } | UpgradeJournalStateV1::Promoted { .. }
    ) {
        return Err(UpgradeError::FinalizedCannotMissCutoff);
    }
    let context = current.lifecycle.context().clone();
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::TerminalMissedCutoff {
            context,
            finalized_height,
            activation_height,
        },
    ))?;
    guard.load()?.ok_or(UpgradeError::CheckpointDisappeared(
        "missed-cutoff checkpoint",
    ))
}
