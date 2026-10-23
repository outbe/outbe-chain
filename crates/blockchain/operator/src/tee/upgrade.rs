//! Owner-only, crash-consistent enclave upgrade checkpoints.
//!
//! The operator never starts or stops Gramine. It records externally completed
//! checkpoints for finalized network-key provisioning and binding transitions.
//! A separate legacy checkpoint supports copying an existing MRSIGNER seal.

use super::journal_storage::{sync_directory, JournalPaths};
use super::JournalSnapshotV1;

mod storage;
use storage::{
    read_private_bounded_file, read_snapshot, validate_directory, validate_private_file,
};

#[cfg(test)]
mod compatibility_tests;

mod context;
mod identity;
mod journal_validation;
mod preparation;
mod relay;
use identity::{
    ensure_transition_source_or_target_v1, transition_target_matches_v1,
    validate_candidate_identity_v1,
};
use journal_validation::{security_material, validate_checkpoint_transition};
use relay::{finalized_transition_matches_v1, prepare_upgrade_relay_v1};
mod submission;
mod wire;

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
use eyre::{Result, WrapErr as _};
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapEvidenceV1,
        RegistrationIntentV1, RegistryMutatorV1, TeeRegistryGasScheduleV1,
        TransitionKeyReadyProofV1,
    },
    tee_registry_abi_v1::ITeeRegistryV1,
};
use outbe_tee::{
    acquire_dcap_collateral_v1, dcap_collateral_validity_window_v1,
    load_replacement_candidate_submission, persist_replacement_candidate_submission,
    ReplacementCandidateEnclaveV1, ReplacementCandidateSubmissionV1,
};
use serde::{Deserialize, Serialize};

use crate::{
    errors::UpgradeError,
    rpc::{RegistryRpc, RelayPreparationRpc, RelayRpc, RenewalRpc},
    tx::{buffered_gas_price, RawRelayTransactionV1, RelaySignerV1},
};

use super::{
    read_finalized_upgrade_policy_v1,
    registry::{read_finalized_bound_renewal_view_v1, NodeBindingSelectorV1},
};

const DIRECTORY: &str = "tee-upgrade-v1";
const SEALED_ROOT: &str = "sealed_root.bin";
const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SEALED_ROOT_BYTES: u64 = 1024 * 1024;
const MAX_RELAY_VARIANTS: usize = 8;

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

pub trait UpgradeNodeSignerV1 {
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65]>;
}

impl<F> UpgradeNodeSignerV1 for F
where
    F: Fn(B256) -> Result<[u8; 65]>,
{
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65]> {
        self(hash)
    }
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
    fn validate(&self) -> Result<()> {
        if self.version != 1 || self.generation == 0 {
            return Err(UpgradeError::UnsupportedVersionOrGeneration.into());
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
) -> Result<Option<UpgradeJournalSnapshotV1>> {
    let paths = JournalPaths::new(node_data_dir, DIRECTORY);
    if !paths.root.exists() {
        return Ok(None);
    }
    validate_directory(&paths.root)?;
    read_snapshot(&paths.journal)
}

pub fn prepare_upgrade_journal_v1(
    node_data_dir: &Path,
    context: UpgradeContextV1,
) -> Result<UpgradeJournalSnapshotV1> {
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
    if let Some(renewal) = super::renewal_journal::inspect_journal(node_data_dir)? {
        if matches!(
            renewal.lifecycle,
            super::renewal_journal::RenewalJournalStateV1::Prepared { .. }
                | super::renewal_journal::RenewalJournalStateV1::Submitted { .. }
        ) {
            return Err(UpgradeError::PendingRenewalBlocksPrepare.into());
        }
    }
    let snapshot =
        UpgradeJournalSnapshotV1::new(UpgradeJournalStateV1::CandidatePrepared { context });
    guard.store(snapshot)?;
    guard
        .load()?
        .ok_or_else(|| UpgradeError::CheckpointDisappeared("prepared upgrade journal").into())
}

pub fn copy_same_platform_sealed_root_and_checkpoint_v1(
    node_data_dir: &Path,
) -> Result<UpgradeJournalSnapshotV1> {
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
            guard.load()?.ok_or_else(|| {
                UpgradeError::CheckpointDisappeared("key-provisioned checkpoint").into()
            })
        }
        UpgradeJournalStateV1::KeyProvisioned {
            context,
            sealed_root_hash,
        } => {
            let actual = copy_same_platform_sealed_root_v1(&context)?;
            if actual != sealed_root_hash {
                return Err(UpgradeError::CandidateSealedRootChanged.into());
            }
            Ok(UpgradeJournalSnapshotV1 {
                lifecycle: UpgradeJournalStateV1::KeyProvisioned {
                    context,
                    sealed_root_hash,
                },
                ..current
            })
        }
        _ => Err(UpgradeError::CopyRequiresCandidatePrepared.into()),
    }
}

/// Record a key that the candidate received from the network and sealed locally.
/// The existing field is the actual new local seal hash, never a source blob hash.
pub fn record_network_key_provisioned_v1(node_data_dir: &Path) -> Result<UpgradeJournalSnapshotV1> {
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::UpgradeNotPrepared)?;
    if let Some(renewal) = super::renewal_journal::inspect_journal(node_data_dir)? {
        if matches!(
            renewal.lifecycle,
            super::renewal_journal::RenewalJournalStateV1::Prepared { .. }
                | super::renewal_journal::RenewalJournalStateV1::Submitted { .. }
        ) {
            return Err(UpgradeError::PendingRenewalBlocksProvision.into());
        }
    }
    let context = current.lifecycle.context().clone();
    let bytes = read_private_bounded_file(
        &context.candidate_tee_dir.join(SEALED_ROOT),
        MAX_SEALED_ROOT_BYTES,
    )?;
    let combined = bytes.starts_with(b"TSGX1");
    #[cfg(feature = "local-e2e")]
    let combined = combined || is_local_e2e_combined_seal(&bytes);
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

// The separate E2E CLI accepts software seals only for the fixed local devnet.
#[cfg(feature = "local-e2e")]
fn is_local_e2e_combined_seal(bytes: &[u8]) -> bool {
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    let start = 5 + 5 + 32; // LE2E1 + TSEAL + canonical seal header.
    bytes.starts_with(b"LE2E1TSEAL")
        && bytes
            .get(start..start + NetworkBindingV1::CANONICAL_LEN)
            .and_then(|v| NetworkBindingV1::decode_canonical(v).ok())
            .is_some_and(|v| {
                v.chain_id
                    == alloy_primitives::U256::from(outbe_primitives::chain::DEVNET_CHAIN_ID)
                        .to_be_bytes::<32>()
                    && v.attestation_mode == AttestationMode::GramineDirectDev
            })
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
    pub fn load_network_submission(&self) -> Result<Option<NetworkUpgradeSubmissionV1>> {
        let path = self.paths.root.join("network-submission.json");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = read_private_bounded_file(&path, MAX_JOURNAL_BYTES)?;
        let value: NetworkUpgradeSubmissionV1 =
            serde_json::from_slice(&bytes).wrap_err("decode upgrade network submission")?;
        value.validate()?;
        Ok(Some(value))
    }
    pub fn store_network_submission(&self, value: &NetworkUpgradeSubmissionV1) -> Result<()> {
        value.validate()?;
        let encoded = serde_json::to_vec(value)?;
        if encoded.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(UpgradeError::NetworkSubmissionExceedsCap.into());
        }
        let next = self.paths.root.join("network-submission.next");
        if next.exists() {
            validate_private_file(&next, MAX_JOURNAL_BYTES)?;
            fs::remove_file(&next)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&next)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        fs::rename(next, self.paths.root.join("network-submission.json"))?;
        sync_directory(&self.paths.root)
    }
}

pub fn record_candidate_key_ready_v1(
    node_data_dir: &Path,
    intent: &RegistrationIntentV1,
    proof: &TransitionKeyReadyProofV1,
    expected_offer_public: [u8; 32],
) -> Result<UpgradeJournalSnapshotV1> {
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
        return Err(UpgradeError::KeyReadyRequiresKeyProvisioned.into());
    };
    if proof.candidate_manifest_hash != context.candidate_manifest_hash {
        return Err(UpgradeError::KeyReadyProofManifestMismatch.into());
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
    guard
        .load()?
        .ok_or_else(|| UpgradeError::CheckpointDisappeared("candidate-key-ready checkpoint").into())
}

pub fn record_upgrade_submission_prepared_v1(
    node_data_dir: &Path,
    submission: PreparedUpgradeSubmissionV1,
) -> Result<UpgradeJournalSnapshotV1> {
    submission.validate()?;
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::CandidateNotPrepared)?;
    let UpgradeJournalStateV1::CandidateKeyReady { context, security } = current.lifecycle else {
        return Err(UpgradeError::SubmissionRequiresKeyReady.into());
    };
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::SubmissionPrepared {
            context,
            security,
            submission,
        },
    ))?;
    guard
        .load()?
        .ok_or_else(|| UpgradeError::CheckpointDisappeared("submission-prepared checkpoint").into())
}

pub fn record_upgrade_submitted_v1(
    node_data_dir: &Path,
    finalized_height: u64,
) -> Result<UpgradeJournalSnapshotV1> {
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::CandidateNotPrepared)?;
    let UpgradeJournalStateV1::SubmissionPrepared {
        context,
        security,
        submission,
    } = current.lifecycle
    else {
        return Err(UpgradeError::RelayRequiresSubmissionPrepared.into());
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
    guard
        .load()?
        .ok_or_else(|| UpgradeError::CheckpointDisappeared("submitted upgrade checkpoint").into())
}

pub fn record_upgrade_finalized_v1(
    node_data_dir: &Path,
    finalized_height: u64,
    finalized_hash: B256,
) -> Result<UpgradeJournalSnapshotV1> {
    if finalized_height == 0 || finalized_hash.is_zero() {
        return Err(UpgradeError::IncompleteFinalizedAuthority.into());
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
        return Err(UpgradeError::FinalizationRequiresSubmitted.into());
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
    guard
        .load()?
        .ok_or_else(|| UpgradeError::CheckpointDisappeared("finalized upgrade checkpoint").into())
}

pub fn record_upgrade_promoted_v1(node_data_dir: &Path) -> Result<UpgradeJournalSnapshotV1> {
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
        return Err(UpgradeError::PromotionRequiresFinalized.into());
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
    guard
        .load()?
        .ok_or_else(|| UpgradeError::CheckpointDisappeared("promoted upgrade checkpoint").into())
}

pub fn record_upgrade_missed_cutoff_v1(
    node_data_dir: &Path,
    finalized_height: u64,
    activation_height: u64,
) -> Result<UpgradeJournalSnapshotV1> {
    if activation_height == 0 || finalized_height < activation_height {
        return Err(UpgradeError::CutoffNotReached.into());
    }
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard.load()?.ok_or(UpgradeError::CandidateNotPrepared)?;
    if matches!(
        current.lifecycle,
        UpgradeJournalStateV1::Finalized { .. } | UpgradeJournalStateV1::Promoted { .. }
    ) {
        return Err(UpgradeError::FinalizedCannotMissCutoff.into());
    }
    let context = current.lifecycle.context().clone();
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::TerminalMissedCutoff {
            context,
            finalized_height,
            activation_height,
        },
    ))?;
    guard
        .load()?
        .ok_or_else(|| UpgradeError::CheckpointDisappeared("missed-cutoff checkpoint").into())
}

/// RPC, relay and node signing capabilities for the replacement candidate.
pub struct UpgradeSubmissionServicesV1<'a, R, N> {
    pub rpc: &'a R,
    pub relay: &'a RelaySignerV1,
    pub candidate: &'a mut ReplacementCandidateEnclaveV1,
    pub node_signer: &'a N,
}

/// Node storage and binding target for one durable upgrade submission.
pub struct UpgradeSubmissionRequestV1<'a> {
    pub node_data_dir: &'a Path,
    pub selector: &'a NodeBindingSelectorV1,
    pub binding_id: B256,
    pub requested_valid_until: u64,
}

/// Resume one same-platform measurement transition from its durable
/// checkpoints. The deployment manager must have already copied the root and
/// restarted candidate B before calling this reducer.
pub async fn run_upgrade_submission_v1<R: RenewalRpc + Sync, N: UpgradeNodeSignerV1>(
    services: UpgradeSubmissionServicesV1<'_, R, N>,
    request: UpgradeSubmissionRequestV1<'_>,
) -> Result<UpgradeSubmissionOutcomeV1> {
    let mut service = submission::UpgradeSubmissionService {
        rpc: services.rpc,
        relay: services.relay,
        candidate: services.candidate,
        node_signer: services.node_signer,
        node_data_dir: request.node_data_dir,
        selector: request.selector,
        binding_id: request.binding_id,
        requested_valid_until: request.requested_valid_until,
    };
    submission::run(&mut service).await
}

async fn reset_expired_upgrade_submission_v1(
    rpc: &(impl RegistryRpc + Sync),
    node_data_dir: &Path,
    selector: &NodeBindingSelectorV1,
    snapshot: &UpgradeJournalSnapshotV1,
) -> Result<bool> {
    let durable = load_replacement_candidate_submission(node_data_dir)?
        .ok_or(UpgradeError::MissingDurableSubmissionEvidence)?;
    let evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
        .map_err(UpgradeError::InvalidTransitionEvidence)?;
    let view = read_finalized_bound_renewal_view_v1(rpc, selector).await?;
    if view.schedule.finalized_timestamp < evidence.intent().requested_valid_until
        || transition_target_matches_v1(&view.binding, evidence.intent())?
    {
        return Ok(false);
    }
    ensure_transition_source_or_target_v1(&view.binding, evidence.intent())?;
    let sealed_root_hash = security_material(&snapshot.lifecycle)
        .ok_or(UpgradeError::MissingSealedRootCheckpoint)?
        .0;
    // Reset first. KeyProvisioned can reconcile a crash while the old evidence still
    // exists, and cannot relay again until a fresh key-ready proof is persisted.
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    if guard.load()?.as_ref() != Some(snapshot) {
        return Err(UpgradeError::ConcurrentChangeDuringRecovery.into());
    }
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::KeyProvisioned {
            context: snapshot.lifecycle.context().clone(),
            sealed_root_hash,
        },
    ))?;
    Ok(true)
}

fn last_transaction_hash(submission: &PreparedUpgradeSubmissionV1) -> Result<B256> {
    submission
        .relay_variants
        .last()
        .map(|raw| raw.transaction_hash)
        .ok_or_else(|| UpgradeError::MissingRelayTransaction.into())
}

fn transaction_is_already_known(error: &eyre::Report) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    message.contains("already known") || message.contains("known transaction")
}

pub struct PreparedTransitionEvidenceV1 {
    pub intent: RegistrationIntentV1,
    pub evidence: AttestationEvidenceV1,
    pub enclave_signature: [u8; 64],
    pub collateral_issue_floor: u64,
    pub collateral_expiration: u64,
}

pub fn generate_transition_evidence_v1(
    candidate: &mut ReplacementCandidateEnclaveV1,
    intent: RegistrationIntentV1,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
) -> Result<PreparedTransitionEvidenceV1> {
    if policy.attestation_mode == AttestationMode::GramineDirectDev {
        let evidence = if intent.operation == AttestationOperationV1::PrepareEnclaveUpgrade {
            let signature = candidate.sign_registration_intent_dev_v1(&intent)?;
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
    active: &super::registry::FinalizedRenewalChainViewV1,
    successor: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    binding_id: B256,
    requested_valid_until: u64,
) -> Result<RegistrationIntentV1> {
    if binding_id.is_zero() || binding_id == active.binding.binding_id {
        return Err(UpgradeError::InvalidTransitionBindingId.into());
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
pub fn copy_same_platform_sealed_root_v1(context: &UpgradeContextV1) -> Result<B256> {
    context.validate()?;
    validate_directory(&context.active_tee_dir)
        .wrap_err("validate active enclave tee directory")?;
    validate_directory(&context.candidate_tee_dir)
        .wrap_err("validate candidate enclave tee directory")?;
    let source = context.active_tee_dir.join(SEALED_ROOT);
    let destination = context.candidate_tee_dir.join(SEALED_ROOT);
    let bytes = read_private_bounded_file(&source, MAX_SEALED_ROOT_BYTES)
        .wrap_err("read active sealed root")?;
    if bytes.is_empty() {
        return Err(UpgradeError::EmptyActiveSealedRoot.into());
    }
    let hash = keccak256(&bytes);
    if destination.exists() {
        let existing = read_private_bounded_file(&destination, MAX_SEALED_ROOT_BYTES)
            .wrap_err("read existing candidate sealed root")?;
        if existing != bytes {
            return Err(UpgradeError::ConflictingCandidateSealedRoot.into());
        }
        return Ok(hash);
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&destination)
        .wrap_err("create candidate sealed root")?;
    output
        .write_all(&bytes)
        .wrap_err("write candidate sealed root")?;
    output.sync_all().wrap_err("fsync candidate sealed root")?;
    sync_directory(&context.candidate_tee_dir)?;
    Ok(hash)
}

#[cfg(test)]
mod tests;
