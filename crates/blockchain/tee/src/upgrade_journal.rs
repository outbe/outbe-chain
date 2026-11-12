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
#[cfg(feature = "native-dcap")]
use outbe_primitives::tee_attestation_v1::{AttestationMode, DcapEvidenceV1};
use outbe_primitives::{
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationOperationV1, RegistrationIntentV1, TeePolicyV1,
        TransitionKeyReadyProofV1,
    },
    tee_registry_abi_v1::ITeeRegistryV1,
};
use serde::{Deserialize, Serialize};

#[cfg(feature = "native-dcap")]
use crate::{acquire_dcap_collateral_v1, dcap_collateral_validity_window_v1};
use crate::{
    journal_storage::{sync_directory, JournalPaths, JournalSnapshotV1},
    registry_view::FinalizedRenewalChainViewV1,
    ReplacementCandidateEnclaveV1, SGX_COMBINED_SEAL_MAGIC,
};

mod context;
mod errors;
mod identity;
mod journal_validation;
mod storage;
mod wire;

#[cfg(test)]
mod compatibility_tests;
#[cfg(test)]
mod tests;

pub use errors::UpgradeError;
pub use identity::{
    ensure_transition_source_or_target_v1, transition_target_matches_v1,
    validate_candidate_identity_v1,
};
use journal_validation::validate_checkpoint_transition;
pub use journal_validation::{security_material, SecurityMaterial};
use storage::{
    read_private_bounded_file, read_snapshot, validate_directory, validate_private_file,
};

const DIRECTORY: &str = "tee-upgrade-v1";
pub const UPGRADE_SEALED_ROOT_FILENAME: &str = "sealed_root.bin";
const SEALED_ROOT: &str = UPGRADE_SEALED_ROOT_FILENAME;
const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SEALED_ROOT_BYTES: u64 = 1024 * 1024;
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

pub fn copy_same_platform_sealed_root_and_checkpoint_v1(
    node_data_dir: &Path,
) -> Result<UpgradeJournalSnapshotV1, UpgradeError> {
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::CandidateNotPrepared)?;
    match current.lifecycle {
        UpgradeJournalStateV1::CandidatePrepared { context } => {
            let sealed_root_hash = copy_same_platform_sealed_root_v1(&context)?;
            guard.store(UpgradeJournalSnapshotV1::new(
                UpgradeJournalStateV1::KeyProvisioned {
                    context,
                    sealed_root_hash,
                },
            ))?;
            guard.load()?.ok_or(UpgradeError::CheckpointDisappeared(
                "key-provisioned checkpoint",
            ))
        }
        UpgradeJournalStateV1::KeyProvisioned {
            context,
            sealed_root_hash,
        } => {
            let actual = copy_same_platform_sealed_root_v1(&context)?;
            if actual != sealed_root_hash {
                return Err(UpgradeError::CandidateSealedRootChanged);
            }
            Ok(UpgradeJournalSnapshotV1 {
                lifecycle: UpgradeJournalStateV1::KeyProvisioned {
                    context,
                    sealed_root_hash,
                },
                ..current
            })
        }
        _ => Err(UpgradeError::CopyRequiresCandidatePrepared),
    }
}

/// Record a key that the candidate received from the network and sealed locally.
pub fn record_network_key_provisioned_with_seal_check_v1<E>(
    node_data_dir: &Path,
    before_read: impl FnOnce() -> Result<(), E>,
    allow_additional_seal: impl FnOnce(&[u8]) -> bool,
) -> Result<UpgradeJournalSnapshotV1, E>
where
    E: From<UpgradeError>,
{
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::UpgradeNotPrepared)?;
    before_read()?;
    let context = current.lifecycle.context().clone();
    let bytes = read_private_bounded_file(
        &context.candidate_tee_dir.join(SEALED_ROOT),
        MAX_SEALED_ROOT_BYTES,
    )?;
    let combined = bytes.starts_with(SGX_COMBINED_SEAL_MAGIC) || allow_additional_seal(&bytes);
    if !combined {
        return Err(UpgradeError::InvalidCombinedSgxSeal.into());
    }
    let sealed_root_hash = keccak256(bytes);
    if let UpgradeJournalStateV1::KeyProvisioned {
        sealed_root_hash: expected,
        ..
    } = current.lifecycle
    {
        if expected != sealed_root_hash {
            return Err(UpgradeError::ProvisionedSealChanged.into());
        }
        return Ok(current);
    }
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::KeyProvisioned {
            context,
            sealed_root_hash,
        },
    ))?;
    guard
        .load()?
        .ok_or_else(|| UpgradeError::CheckpointDisappeared("provisioned journal").into())
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

pub struct PreparedTransitionEvidenceV1 {
    pub intent: RegistrationIntentV1,
    pub evidence: AttestationEvidenceV1,
    pub enclave_signature: [u8; 64],
    pub collateral_issue_floor: u64,
    pub collateral_expiration: u64,
}

#[cfg(feature = "native-dcap")]
pub fn generate_transition_evidence_v1(
    candidate: &mut ReplacementCandidateEnclaveV1,
    intent: RegistrationIntentV1,
    policy: &TeePolicyV1,
) -> Result<PreparedTransitionEvidenceV1, UpgradeError> {
    if policy.attestation_mode == AttestationMode::GramineDirectDev {
        let evidence = if intent.operation == AttestationOperationV1::PrepareEnclaveUpgrade {
            let signature = candidate
                .sign_registration_intent_dev_v1(&intent)
                .map_err(UpgradeError::GenerateDirectDevEvidence)?;
            AttestationEvidenceV1::GramineDirectDev(
                outbe_primitives::tee_attestation_v1::GramineDirectEvidenceV1 {
                    intent: intent.clone(),
                    dev_attestation_public: intent.attestation_ed25519,
                    dev_signature: signature,
                    transition_key_ready_proof: None,
                },
            )
        } else {
            candidate
                .generate_transition_evidence_dev_v1(&intent)
                .map_err(UpgradeError::GenerateDirectDevEvidence)?
        };
        let AttestationEvidenceV1::GramineDirectDev(dev) = &evidence else {
            unreachable!();
        };
        return Ok(PreparedTransitionEvidenceV1 {
            intent,
            enclave_signature: dev.dev_signature,
            evidence,
            collateral_issue_floor: 0,
            collateral_expiration: u64::MAX,
        });
    }
    let generated = candidate
        .generate_dcap_quote(&intent)
        .map_err(UpgradeError::GenerateTransitionQuote)?;
    let components = acquire_dcap_collateral_v1(&generated.quote_body)
        .map_err(UpgradeError::AcquireTransitionCollateral)?;
    let evidence = DcapEvidenceV1 {
        intent: intent.clone(),
        quote: generated.quote_body,
        components,
        transition_key_ready_proof: generated.transition_key_ready_proof,
    };
    let window = dcap_collateral_validity_window_v1(&evidence, policy)
        .map_err(UpgradeError::ValidateTransitionCollateral)?;
    Ok(PreparedTransitionEvidenceV1 {
        intent,
        evidence: AttestationEvidenceV1::Dcap(evidence),
        enclave_signature: generated.enclave_signature,
        collateral_issue_floor: window.issue_floor,
        collateral_expiration: window.expiration_ceiling,
    })
}

pub fn transition_intent_v1(
    candidate: &ReplacementCandidateEnclaveV1,
    active: &FinalizedRenewalChainViewV1,
    successor: &TeePolicyV1,
    binding_id: B256,
    requested_valid_until: u64,
) -> Result<RegistrationIntentV1, UpgradeError> {
    if binding_id.is_zero() || binding_id == active.binding.binding_id {
        return Err(UpgradeError::InvalidTransitionBindingId);
    }
    let manifest = candidate.manifest();
    let intent = RegistrationIntentV1 {
        chain_id: successor.chain_id,
        genesis_hash: successor.genesis_hash,
        operation: AttestationOperationV1::TransitionEnclaveMeasurement,
        attestation_mode: successor.attestation_mode,
        policy_hash: successor
            .policy_hash()
            .map_err(UpgradeError::HashStagedSuccessorPolicy)?,
        node_id: manifest.node_id.clone(),
        enclave_id: manifest
            .enclave_id()
            .map_err(UpgradeError::DeriveCandidateEnclaveId)?,
        binding_id,
        binding_version: active
            .binding
            .binding_version
            .checked_add(1)
            .ok_or(UpgradeError::BindingVersionExhausted)?,
        registration_version: active
            .binding
            .registration_version
            .checked_add(1)
            .ok_or(UpgradeError::RegistrationVersionExhausted)?,
        renewal_nonce: active.binding.renewal_nonce,
        transition_nonce: active
            .binding
            .transition_nonce
            .checked_add(1)
            .ok_or(UpgradeError::TransitionNonceExhausted)?,
        requested_valid_until,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: active.binding.node_host_authorization_hash,
    };
    manifest
        .validate_intent_binding(&intent)
        .map_err(UpgradeError::CandidateManifestIntentMismatch)?;
    Ok(intent)
}

/// Copy exactly `sealed_root.bin` from active A to prepared candidate B.
/// A byte-identical destination is an idempotent crash retry. Any other
/// pre-existing destination fails closed.
pub fn copy_same_platform_sealed_root_v1(context: &UpgradeContextV1) -> Result<B256, UpgradeError> {
    context.validate()?;
    validate_directory(&context.active_tee_dir)?;
    validate_directory(&context.candidate_tee_dir)?;
    let source = context.active_tee_dir.join(SEALED_ROOT);
    let destination = context.candidate_tee_dir.join(SEALED_ROOT);
    let bytes = read_private_bounded_file(&source, MAX_SEALED_ROOT_BYTES)?;
    if bytes.is_empty() {
        return Err(UpgradeError::EmptyActiveSealedRoot);
    }
    let hash = keccak256(&bytes);
    if destination.exists() {
        let existing = read_private_bounded_file(&destination, MAX_SEALED_ROOT_BYTES)?;
        if existing != bytes {
            return Err(UpgradeError::ConflictingCandidateSealedRoot);
        }
        return Ok(hash);
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&destination)
        .map_err(|source| UpgradeError::Io {
            context: "create candidate sealed root",
            source,
        })?;
    output
        .write_all(&bytes)
        .map_err(|source| UpgradeError::Io {
            context: "write candidate sealed root",
            source,
        })?;
    output.sync_all().map_err(|source| UpgradeError::Io {
        context: "fsync candidate sealed root",
        source,
    })?;
    sync_directory(&context.candidate_tee_dir)?;
    Ok(hash)
}
