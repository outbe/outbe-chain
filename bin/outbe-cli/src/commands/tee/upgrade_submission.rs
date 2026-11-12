//! CLI-owned same-platform enclave upgrade submission reducer and renewal-checked wrappers.

mod preparation;
mod relay;
mod submission;

#[cfg(test)]
mod tests;

use std::path::Path;

use alloy_primitives::{keccak256, B256, U256};
use alloy_sol_types::SolCall as _;
use eyre::{Result, WrapErr as _};
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationOperationV1, RegistryMutatorV1, TeeRegistryGasScheduleV1,
    },
    tee_registry_abi_v1::ITeeRegistryV1,
};
use outbe_tee::{
    load_replacement_candidate_submission, persist_replacement_candidate_submission,
    registry_view::{
        read_finalized_bound_renewal_view_v1, read_finalized_upgrade_policy_v1,
        NodeBindingSelectorV1, RegistryRpc,
    },
    upgrade_journal::{
        ensure_transition_source_or_target_v1, generate_transition_evidence_v1,
        inspect_upgrade_journal_v1, prepare_upgrade_journal_with_check_v1,
        record_candidate_key_ready_v1, record_network_key_provisioned_with_seal_check_v1,
        record_upgrade_submission_prepared_v1, record_upgrade_submitted_v1, security_material,
        transition_intent_v1, transition_target_matches_v1, validate_candidate_identity_v1,
        PreparedTransitionEvidenceV1, PreparedUpgradeSubmissionV1, UpgradeContextV1, UpgradeError,
        UpgradeJournalGuardV1, UpgradeJournalSnapshotV1, UpgradeJournalStateV1,
        UpgradeSubmissionOutcomeV1,
    },
    ReplacementCandidateEnclaveV1, ReplacementCandidateSubmissionV1,
};

use super::{
    relay_tx::{buffered_gas_price, RawRelayTransactionV1, RelaySignerV1},
    rpc::{RelayPreparationRpc, RelayRpc, RenewalRpc},
};

use relay::{finalized_transition_matches_v1, prepare_upgrade_relay_v1};

pub(super) trait UpgradeNodeSignerV1 {
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

/// RPC, relay and node signing capabilities for the replacement candidate.
pub(super) struct UpgradeSubmissionServicesV1<'a, R, N> {
    pub rpc: &'a R,
    pub relay: &'a RelaySignerV1,
    pub candidate: &'a mut ReplacementCandidateEnclaveV1,
    pub node_signer: &'a N,
}

/// Node storage and binding target for one durable upgrade submission.
pub(super) struct UpgradeSubmissionRequestV1<'a> {
    pub node_data_dir: &'a Path,
    pub selector: &'a NodeBindingSelectorV1,
    pub binding_id: B256,
    pub requested_valid_until: u64,
}

pub(super) fn prepare_upgrade_journal_v1(
    node_data_dir: &Path,
    context: UpgradeContextV1,
) -> Result<UpgradeJournalSnapshotV1> {
    prepare_upgrade_journal_with_check_v1(node_data_dir, context, || {
        if let Some(renewal) = super::renewal_journal::inspect_journal(node_data_dir)? {
            if matches!(
                renewal.lifecycle,
                super::renewal_journal::RenewalJournalStateV1::Prepared { .. }
                    | super::renewal_journal::RenewalJournalStateV1::Submitted { .. }
            ) {
                return Err(UpgradeError::PendingRenewalBlocksPrepare.into());
            }
        }
        Ok(())
    })
}

/// Record a key that the candidate received from the network and sealed locally.
/// The existing field is the actual new local seal hash, never a source blob hash.
pub(super) fn record_network_key_provisioned_v1(
    node_data_dir: &Path,
) -> Result<UpgradeJournalSnapshotV1> {
    record_network_key_provisioned_with_seal_check_v1(
        node_data_dir,
        || {
            if let Some(renewal) = super::renewal_journal::inspect_journal(node_data_dir)? {
                if matches!(
                    renewal.lifecycle,
                    super::renewal_journal::RenewalJournalStateV1::Prepared { .. }
                        | super::renewal_journal::RenewalJournalStateV1::Submitted { .. }
                ) {
                    return Err(UpgradeError::PendingRenewalBlocksProvision.into());
                }
            }
            Ok(())
        },
        |bytes| {
            #[cfg(feature = "local-e2e")]
            {
                is_local_e2e_combined_seal(bytes)
            }
            #[cfg(not(feature = "local-e2e"))]
            {
                let _ = bytes;
                false
            }
        },
    )
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
                    == U256::from(outbe_primitives::chain::DEVNET_CHAIN_ID).to_be_bytes::<32>()
                    && v.attestation_mode == AttestationMode::GramineDirectDev
            })
}

/// Resume one same-platform measurement transition from its durable
/// checkpoints. The deployment manager must have already copied the root and
/// restarted candidate B before calling this reducer.
pub(super) async fn run_upgrade_submission_v1<R: RenewalRpc + Sync, N: UpgradeNodeSignerV1>(
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
