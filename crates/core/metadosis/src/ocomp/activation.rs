use alloy_primitives::{Bytes, B256, U256};
use outbe_compressed_entities::ExecutionScope;
use outbe_intex::{install_certified_contributor_root, CertifiedContributorRootV1};
use outbe_lysis::activation_v1::{self, LysisApplyPlanV1, LysisOwnerReceiptsV1};
use outbe_nod::schema::NodContract;
use outbe_nodfactory::certified::{install_certified_generation, CertifiedNodGenerationV1};
use outbe_ocomp_protocol::{
    abi::OCOMP_ACTIVATION_REJECTED_SELECTOR,
    error::ProtocolError,
    hash::hash_framed,
    intent::{
        ExpectedFinalizedIntentBindingV1, FinalizedIntentProofV1, FinalizedIntentVerificationError,
        VerifiedFinalizedIntentV1,
    },
    profile::ProtocolBundleV1,
    receipts::{
        empty_apply_event_summary_hash, ActivationOutcome, AggregateActivationReceiptV1,
        RequestBudgetSplitReceiptV1,
    },
    registry::HashDomain,
    state::{ActiveGenerationV1, OcompCompletedBindingV1, OcompJobRecordV1, OcompJobStatus},
    SchemaLimits, OCB1_HEADER_LEN,
};
use outbe_primitives::error::{PrecompileError, Result as PrecompileResult};
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::certified::{credit_certified_carry_over, CertifiedCarryOverCreditV1};
use outbe_tribute::{
    certified::{retire_certified_partition, CertifiedTributeRetirementV1},
    TributeContract,
};

use crate::reducer::OuterWwdTransition;
use crate::{precompile::IMetadosis, schema::MetadosisContract};

use super::{profile::OcompRequestProfile, state::JobFsmLimits};

const REJECT_LIMIT_EXCEEDED: u16 = 2;
const REJECT_FORK_OR_BUNDLE_MISMATCH: u16 = 3;
const REJECT_JOB_BINDING_INVALID: u16 = 9;
const REJECT_COMMITTEE_SNAPSHOT_INVALID: u16 = 10;
const REJECT_RESULT_DIGEST_MISMATCH: u16 = 12;
const REJECT_RESULT_STRUCTURE_INVALID: u16 = 13;
const REJECT_OWNER_APPLY_REJECTED: u16 = 16;
const REJECT_RECEIPT_MISMATCH: u16 = 17;

/// Complete immutable consensus authority used by public LYSIS_V1 activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompActivationAuthorityV1 {
    pub bundle: ProtocolBundleV1,
}

/// Consensus finality authority supplied by the node execution environment.
///
/// The Metadosis activation coordinator owns protocol ordering and state
/// transitions, while this narrow seam owns canonical-header, historical
/// consensus-committee and authenticated intent-inclusion verification.
/// Relay data can never implement or replace this authority.
pub trait OcompFinalizedIntentAuthority: Send + Sync {
    fn verify(
        &self,
        proof: &FinalizedIntentProofV1,
        expected: ExpectedFinalizedIntentBindingV1,
        limits: &SchemaLimits,
    ) -> std::result::Result<VerifiedFinalizedIntentV1, OcompFinalityAuthorityError>;
}

/// Separates relayer-controlled invalid evidence from failures of the
/// node-owned canonical/finalized anchor. The former is a stable transaction
/// rejection; the latter must stop block execution rather than let local
/// readiness silently redefine validity.
#[derive(Debug, thiserror::Error)]
pub enum OcompFinalityAuthorityError {
    #[error(transparent)]
    InvalidProof(#[from] FinalizedIntentVerificationError),
    #[error("node-owned finalized authority unavailable: {0}")]
    LocalAuthority(String),
}

/// Node-local authority proving that this process independently computed the
/// exact canonical Lysis result carried by the q-forming vote.
///
/// This is deliberately not consensus state: every validator and FullNode must
/// reach the same result independently before it accepts the terminal state
/// transition. An unavailable or disagreeing authority therefore stops local
/// block execution instead of turning into a public transaction rejection.
pub trait OcompLocalResultAuthority: Send + Sync {
    fn verify_exact(
        &self,
        job_id: B256,
        result: &outbe_ocomp_protocol::result::LysisResultV1,
        limits: &SchemaLimits,
    ) -> std::result::Result<(), OcompLocalResultAuthorityError>;
}

#[derive(Debug, thiserror::Error)]
pub enum OcompLocalResultAuthorityError {
    #[error("local Lysis result is missing for job {job_id}")]
    Missing { job_id: B256 },
    #[error("local Lysis result mismatch for job {job_id}")]
    Mismatch { job_id: B256 },
    #[error("node-local Lysis result authority unavailable: {0}")]
    Unavailable(String),
}

#[derive(Clone, Copy)]
pub(crate) struct QuorumApplyContext<'a, 'storage> {
    storage: &'a StorageHandle<'storage>,
    scope: &'a ExecutionScope,
    completed_transition: &'a OuterWwdTransition,
    conflict_transition: &'a OuterWwdTransition,
    current_height: u64,
    current_time: u64,
    limits: &'a SchemaLimits,
}

impl<'a, 'storage> QuorumApplyContext<'a, 'storage> {
    pub(crate) const fn new(
        storage: &'a StorageHandle<'storage>,
        scope: &'a ExecutionScope,
        completed_transition: &'a OuterWwdTransition,
        conflict_transition: &'a OuterWwdTransition,
        current_height: u64,
        current_time: u64,
        limits: &'a SchemaLimits,
    ) -> Self {
        Self {
            storage,
            scope,
            completed_transition,
            conflict_transition,
            current_height,
            current_time,
            limits,
        }
    }
}

struct CertifiedResultInput<'a> {
    result: &'a outbe_ocomp_protocol::result::LysisResultV1,
    bundle: &'a ProtocolBundleV1,
    plan: &'a LysisApplyPlanV1,
    quorum: &'a outbe_ocomp_protocol::vote::OcompQuorumV1,
    result_evidence_hash: B256,
}

pub(crate) struct QuorumResultInput<'a> {
    intent_id: B256,
    record: &'a OcompJobRecordV1,
    result: &'a outbe_ocomp_protocol::result::LysisResultV1,
    quorum: &'a outbe_ocomp_protocol::vote::OcompQuorumV1,
    authority: &'a OcompActivationAuthorityV1,
    local_result_authority: Option<&'a dyn OcompLocalResultAuthority>,
}

impl<'a> QuorumResultInput<'a> {
    pub(crate) const fn new(
        intent_id: B256,
        record: &'a OcompJobRecordV1,
        result: &'a outbe_ocomp_protocol::result::LysisResultV1,
        quorum: &'a outbe_ocomp_protocol::vote::OcompQuorumV1,
        authority: &'a OcompActivationAuthorityV1,
        local_result_authority: Option<&'a dyn OcompLocalResultAuthority>,
    ) -> Self {
        Self {
            intent_id,
            record,
            result,
            quorum,
            authority,
            local_result_authority,
        }
    }
}

/// Verifies and applies the full result carried by the vote that first reaches
/// the intent's pinned quorum. The caller owns the outer storage checkpoint
/// that also contains the q-forming vote slot and quorum, so any verifier/owner
/// failure rolls the complete transition back.
pub(crate) fn apply_quorum_result(
    context: QuorumApplyContext<'_, '_>,
    metadosis: &mut MetadosisContract<'_>,
    input: QuorumResultInput<'_>,
) -> PrecompileResult<Bytes> {
    let QuorumResultInput {
        intent_id,
        record,
        result,
        quorum,
        authority,
        local_result_authority,
    } = input;
    let current_height = context.current_height;
    let limits = context.limits;
    if record.status != OcompJobStatus::VotingOpen || record.terminal.is_some() {
        return Err(fatal(
            "OCOMP q-forming result requires a voting-open non-terminal job",
        ));
    }
    let finalized = record
        .finalized
        .as_ref()
        .ok_or_else(|| fatal("OCOMP q-forming job is not finalized"))?;
    if finalized.quorum.is_some() {
        return Err(fatal("OCOMP q-forming job already has a quorum"));
    }

    let profile = metadosis
        .read_ocomp_request_profile(limits)?
        .ok_or_else(|| fatal("pending OCOMP job has no request profile"))?;
    if record.intent.chain_id != profile.chain_id
        || record.intent.genesis_hash != profile.genesis_hash
        || record.intent.fork_id != profile.fork_id
        || record.intent.protocol_bundle_hash != profile.protocol_bundle_hash
    {
        return Err(reject(REJECT_FORK_OR_BUNDLE_MISMATCH));
    }
    let historical_snapshot = outbe_validatorset::read_ocomp_snapshot_extension_for_binding(
        context.storage.clone(),
        record.intent.result_validator_set_epoch,
        record.intent.result_committee_set_hash,
        record.intent.result_ocomp_binding_hash,
    )?
    .filter(|snapshot| snapshot.member_count == record.intent.result_member_count);
    if historical_snapshot.is_none() {
        return Err(reject(REJECT_COMMITTEE_SNAPSHOT_INVALID));
    }

    let record_intent_id = record
        .intent
        .intent_id(limits)
        .map_err(|error| fatal(format!("hash live OCOMP intent: {error}")))?;
    if intent_id != record_intent_id
        || result.job_id != finalized.job_id
        || result.attempt != record.intent.attempt
        || result.protocol_bundle_hash != record.intent.protocol_bundle_hash
    {
        return Err(reject(REJECT_JOB_BINDING_INVALID));
    }
    result
        .validate_finalized_intent(&record.intent)
        .map_err(|error| protocol_reject(error, REJECT_JOB_BINDING_INVALID))?;
    let result_digest = result
        .result_digest(limits)
        .map_err(|error| protocol_reject(error, REJECT_RESULT_STRUCTURE_INVALID))?;
    if quorum.result_digest != result_digest || quorum.quorum_height != current_height {
        return Err(reject(REJECT_RESULT_DIGEST_MISMATCH));
    }
    local_result_authority
        .ok_or_else(|| fatal("node-local Lysis result authority is unavailable"))?
        .verify_exact(finalized.job_id, result, limits)
        .map_err(|error| {
            fatal(format!(
                "node-local Lysis result verification failed: {error}"
            ))
        })?;
    let result_evidence_hash = result
        .result_evidence_hash(limits)
        .map_err(|error| protocol_reject(error, REJECT_RESULT_STRUCTURE_INVALID))?;
    let activation_payload = result
        .activation_payload(limits)
        .map_err(|error| protocol_reject(error, REJECT_RESULT_STRUCTURE_INVALID))?;
    let plan = activation_v1::verify_result(
        intent_id,
        finalized.job_id,
        &record.intent,
        &activation_payload,
        result,
        limits,
    )
    .map_err(|error| protocol_reject(error, REJECT_RESULT_STRUCTURE_INVALID))?;

    let job_fsm_limits = profile.fsm_limits();
    if target_preconditions_changed(context, metadosis, record, job_fsm_limits)? {
        commit_conflict(
            context,
            metadosis,
            &plan,
            quorum,
            result_evidence_hash,
            job_fsm_limits,
        )
    } else {
        let certified = CertifiedResultInput {
            result,
            bundle: &authority.bundle,
            plan: &plan,
            quorum,
            result_evidence_hash,
        };
        apply_certified_result(context, metadosis, certified, job_fsm_limits)
    }
}

fn target_preconditions_changed(
    context: QuorumApplyContext<'_, '_>,
    metadosis: &MetadosisContract<'_>,
    record: &OcompJobRecordV1,
    fsm_limits: JobFsmLimits,
) -> PrecompileResult<bool> {
    let storage = context.storage;
    let limits = context.limits;
    let expected = &record.intent.activation_preconditions;
    let wwd = outbe_common::WorldwideDay::new(record.intent.wwd);
    let tribute = TributeContract::new(storage.clone()).pre_admission_projection(wwd)?;
    let nod = NodContract::new(storage.clone()).ocomp_target_projection(wwd)?;
    let contributors = outbe_intex::api::ocomp_contributor_target_projection(
        storage,
        expected.contributors.series_id,
    )?;
    let metadosis_projection = metadosis.ocomp_pre_admission_projection(wwd)?;
    let intent_id = record
        .intent
        .intent_id(limits)
        .map_err(|error| fatal(format!("hash target OCOMP intent: {error}")))?;
    let fsm = metadosis
        .live_ocomp_fsm_state_by_intent(intent_id, limits, fsm_limits)?
        .ok_or_else(|| fatal("pending OCOMP job has no live FSM state"))?
        .projection();
    let status = metadosis.get_wwd_status(wwd)?;

    Ok(!tribute.profile_ready
        || !tribute.is_sealed
        || tribute.source_generation != expected.tribute.source_generation
        || tribute.sealed_collection_root != expected.tribute.sealed_collection_root
        || tribute.tribute_count != expected.tribute.exact_count
        || tribute.tribute_nominal_amount != expected.tribute.exact_nominal_total
        || nod.worldwide_day != wwd
        || nod.target_generation != expected.nod.target_generation
        || nod.namespace_root_before != expected.nod.namespace_root_before
        || contributors.series_id != expected.contributors.series_id
        || contributors.expected_series_version != expected.contributors.expected_series_version
        || contributors.contributor_count != 0
        || !contributors.contributor_total.is_zero()
        || !metadosis_projection.initialized
        || metadosis_projection.state_version != expected.metadosis.state_version
        || status != crate::aggregate::WwdStatus::OffchainPending
        || fsm.live_intent_id != Some(intent_id)
        || fsm.pending_nonce != expected.metadosis.pending_nonce)
}

fn commit_conflict(
    context: QuorumApplyContext<'_, '_>,
    metadosis: &mut MetadosisContract<'_>,
    plan: &LysisApplyPlanV1,
    quorum: &outbe_ocomp_protocol::vote::OcompQuorumV1,
    result_evidence_hash: B256,
    fsm_limits: JobFsmLimits,
) -> PrecompileResult<Bytes> {
    let outer_transition = context.conflict_transition;
    let current_height = context.current_height;
    let current_time = context.current_time;
    let limits = context.limits;
    let binding = plan.binding().clone();
    let receipt = AggregateActivationReceiptV1 {
        binding: binding.clone(),
        outcome: ActivationOutcome::ConflictResolved,
        nod_receipt_hash: None,
        contributor_receipt_hash: None,
        tribute_receipt_hash: None,
        carry_over_receipt_hash: None,
        request_budget_split_receipt_hash: plan.request_budget_split_receipt_hash(),
        active_generation_hash: None,
        effect_commitment: hash_framed(HashDomain::Effects, &[])
            .map_err(|error| fatal(format!("hash empty conflict effects: {error}")))?,
        event_summary_hash: empty_apply_event_summary_hash()
            .map_err(|error| fatal(format!("hash empty conflict event summary: {error}")))?,
        activated_at_height: current_height,
        activated_at_time: current_time,
    };
    let terminal_receipt_hash = receipt
        .terminal_receipt_hash(limits)
        .map_err(|error| fatal(format!("hash conflict receipt: {error}")))?;
    let completed_binding = OcompCompletedBindingV1 {
        job_id: binding.job_id,
        activation_call_id: binding.activation_call_id,
        result_digest: binding.result_digest,
        quorum_height: quorum.quorum_height,
        quorum_signer_bitmap: quorum.signer_bitmap.clone(),
        quorum_evidence_hash: quorum.evidence_hash,
        result_evidence_hash,
        terminal_receipt_hash,
        terminal_receipt: receipt,
    };
    let next_pending_nonce = metadosis.commit_ocomp_conflict(
        outer_transition,
        binding.intent_id,
        completed_binding,
        quorum,
        current_height,
        current_time,
        limits,
        fsm_limits,
    )?;
    metadosis.emit(IMetadosis::OffchainJobConflicted {
        intentId: binding.intent_id,
        jobId: binding.job_id,
        attempt: binding.attempt,
        oldPendingNonce: plan.call_core().terminal_pending_nonce,
        nextPendingNonce: next_pending_nonce,
        resultDigest: binding.result_digest,
    })?;
    Ok(encode_activation_return(
        binding.activation_call_id,
        binding.result_digest,
        ActivationOutcome::ConflictResolved,
    ))
}

fn apply_certified_result(
    context: QuorumApplyContext<'_, '_>,
    metadosis: &mut MetadosisContract<'_>,
    certified: CertifiedResultInput<'_>,
    fsm_limits: JobFsmLimits,
) -> PrecompileResult<Bytes> {
    let storage = context.storage;
    let scope = context.scope;
    let outer_transition = context.completed_transition;
    let current_height = context.current_height;
    let current_time = context.current_time;
    let limits = context.limits;
    let CertifiedResultInput {
        result,
        bundle,
        plan,
        quorum,
        result_evidence_hash,
    } = certified;
    let binding = plan.binding().clone();
    let mut request_receipt = metadosis
        .request_budget_receipt(
            outbe_common::WorldwideDay::new(plan.carry_over().source_wwd()),
            limits,
        )?
        .ok_or_else(|| fatal("OCOMP request budget receipt is missing"))?;
    let active_generation = ActiveGenerationV1 {
        job_id: binding.job_id,
        program_semantics_hash: bundle.lysis_program_semantics_hash,
        nod_root: plan.nod().nod_root(),
        bucket_root: plan.nod().bucket_root(),
        contributor_root: plan.contributors().contributor_root(),
        output_manifest_root: plan.nod().output_manifest_root(),
        exact_counts: plan.nod().exact_counts().clone(),
        result_evidence_hash,
        availability_certificate_hash: None,
    };
    let nod_input = CertifiedNodGenerationV1 {
        binding: binding.clone(),
        precondition: plan.nod().precondition().clone(),
        roots: result.roots.clone(),
        counts: plan.nod().exact_counts().clone(),
        nod_amount_total: plan.nod().nod_amount_total(),
        nod_gratis_consumed: plan.nod().nod_gratis_consumed(),
        issued_at: plan.nod().issued_at(),
    };
    let contributor_input = CertifiedContributorRootV1 {
        binding: binding.clone(),
        precondition: plan.contributors().precondition().clone(),
        contributor_root: plan.contributors().contributor_root(),
        contributor_count: plan.contributors().contributor_count(),
        eligible_nominal_total: plan.contributors().eligible_nominal_total(),
    };
    let tribute_input = CertifiedTributeRetirementV1 {
        binding: binding.clone(),
        input_binding: plan.tribute().input_binding().clone(),
        consumed_count: plan.tribute().consumed_count(),
        consumed_nominal_total: plan.tribute().consumed_nominal_total(),
        retired_generation: plan.tribute().retired_generation(),
    };
    let lysis_budget = plan
        .nod()
        .nod_gratis_consumed()
        .checked_add(plan.carry_over().credited_unused_lysis())
        .ok_or_else(|| reject(REJECT_RESULT_STRUCTURE_INVALID))?;
    let carry_over_input = CertifiedCarryOverCreditV1 {
        binding: binding.clone(),
        source_wwd: plan.carry_over().source_wwd(),
        lysis_budget,
        nod_gratis_consumed: plan.nod().nod_gratis_consumed(),
        unused_lysis: plan.carry_over().credited_unused_lysis(),
    };

    storage.with_lysis_activation_frame(binding.activation_call_id, |capability| {
        let nod = install_certified_generation(storage, capability, &nod_input, limits)
            .map_err(owner_apply_error)?;
        let contributor =
            install_certified_contributor_root(storage, capability, &contributor_input, limits)
                .map_err(owner_apply_error)?;
        let tribute =
            retire_certified_partition(storage, scope, capability, &tribute_input, limits)
                .map_err(owner_apply_error)?;
        let carry_over =
            credit_certified_carry_over(storage, capability, &carry_over_input, limits)
                .map_err(owner_apply_error)?;
        let mut receipts = LysisOwnerReceiptsV1 {
            nod,
            contributor,
            tribute,
            carry_over,
        };
        inject_test_receipt_fault(&mut request_receipt, &mut receipts);
        let verified_receipts =
            activation_v1::verify_receipts(plan, &request_receipt, &receipts, limits)
                .map_err(|_| reject(REJECT_RECEIPT_MISMATCH))?;
        let permit = verified_receipts
            .terminal_permit(capability)
            .map_err(|_| reject(REJECT_RECEIPT_MISMATCH))?;
        let completed = metadosis.commit_ocomp_completed(
            outer_transition,
            binding.intent_id,
            active_generation,
            result_evidence_hash,
            plan.nod().nod_gratis_consumed(),
            plan.carry_over().credited_unused_lysis(),
            current_height,
            current_time,
            permit,
            quorum,
            limits,
            fsm_limits,
        )?;
        Ok(encode_activation_return(
            completed.activation_call_id,
            completed.result_digest,
            ActivationOutcome::Applied,
        ))
    })
}

fn inject_test_receipt_fault(
    request_receipt: &mut RequestBudgetSplitReceiptV1,
    receipts: &mut LysisOwnerReceiptsV1,
) {
    #[cfg(test)]
    crate::fixture_kernel::inject_receipt_fault(request_receipt, receipts);
    #[cfg(not(test))]
    let _ = (request_receipt, receipts);
}

fn owner_apply_error(error: PrecompileError) -> PrecompileError {
    match error {
        PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => {
            reject(REJECT_OWNER_APPLY_REJECTED)
        }
        other => other,
    }
}

fn protocol_reject(error: ProtocolError, fallback: u16) -> PrecompileError {
    match error {
        ProtocolError::CapacityExceeded { .. } | ProtocolError::AllocationFailed { .. } => {
            reject(REJECT_LIMIT_EXCEEDED)
        }
        _ => reject(fallback),
    }
}

fn encode_activation_return(
    activation_call_id: B256,
    result_digest: B256,
    outcome: ActivationOutcome,
) -> Bytes {
    let mut encoded = Vec::with_capacity(96);
    encoded.extend_from_slice(activation_call_id.as_slice());
    encoded.extend_from_slice(result_digest.as_slice());
    encoded.extend_from_slice(&U256::from(outcome as u8).to_be_bytes::<32>());
    Bytes::from(encoded)
}

fn reject(code: u16) -> PrecompileError {
    let mut encoded = Vec::with_capacity(36);
    encoded.extend_from_slice(&OCOMP_ACTIVATION_REJECTED_SELECTOR);
    encoded.extend_from_slice(&U256::from(code).to_be_bytes::<32>());
    PrecompileError::RevertBytes(Bytes::from(encoded))
}

impl MetadosisContract<'_> {
    /// Installs the immutable protocol bundle exactly once. Voting membership
    /// comes only from each job's pinned ValidatorSet snapshot. The former
    /// committee storage field stays reserved and must remain zero.
    pub fn initialize_ocomp_activation_authority(
        &mut self,
        bundle: &ProtocolBundleV1,
        limits: &SchemaLimits,
    ) -> PrecompileResult<()> {
        (|| {
            let profile = self
                .read_ocomp_request_profile(limits)?
                .ok_or_else(|| fatal("OCOMP request profile is not initialized"))?;
            validate_activation_authority(&profile, bundle, limits)?;
            if !self.ocomp_result_committee_snapshot.is_empty()? {
                return Err(fatal("reserved OCOMP committee slot is non-zero"));
            }

            match self.read_ocomp_activation_authority(limits)? {
                Some(existing) if existing.bundle == *bundle => Ok(()),
                Some(_) => Err(fatal("OCOMP activation authority is immutable")),
                None => {
                    self.ocomp_active_protocol_bundle
                        .write(&bundle.encode_canonical(limits).map_err(protocol_error)?)?;
                    if self.read_ocomp_activation_authority(limits)?
                        != Some(OcompActivationAuthorityV1 {
                            bundle: bundle.clone(),
                        })
                    {
                        return Err(fatal("OCOMP activation authority write/read mismatch"));
                    }
                    Ok(())
                }
            }
        })()
    }

    pub fn read_ocomp_activation_authority(
        &self,
        limits: &SchemaLimits,
    ) -> PrecompileResult<Option<OcompActivationAuthorityV1>> {
        let bundle_len = self.ocomp_active_protocol_bundle.len()?;
        if !self.ocomp_result_committee_snapshot.is_empty()? {
            return Err(fatal("reserved OCOMP committee slot is non-zero"));
        }
        if bundle_len == 0 {
            return Ok(None);
        }
        let max = limits
            .codec
            .max_body_bytes
            .checked_add(OCB1_HEADER_LEN)
            .ok_or_else(|| fatal("OCOMP activation authority byte cap overflow"))?;
        if bundle_len > max {
            return Err(fatal("OCOMP activation authority exceeds byte cap"));
        }
        let bundle =
            ProtocolBundleV1::decode_canonical(&self.ocomp_active_protocol_bundle.read()?, limits)
                .map_err(protocol_error)?;
        let profile = self
            .read_ocomp_request_profile(limits)?
            .ok_or_else(|| fatal("OCOMP activation authority has no request profile"))?;
        validate_activation_authority(&profile, &bundle, limits)?;
        Ok(Some(OcompActivationAuthorityV1 { bundle }))
    }
}

pub(crate) fn validate_activation_authority(
    profile: &OcompRequestProfile,
    bundle: &ProtocolBundleV1,
    limits: &SchemaLimits,
) -> PrecompileResult<()> {
    let bundle_hash = bundle
        .protocol_bundle_hash(limits)
        .map_err(protocol_error)?;
    if bundle_hash != profile.protocol_bundle_hash
        || bundle.fork_id != profile.fork_id
        || bundle.correctness_profile_id != profile.correctness_profile_id
        || bundle.capacity_profile_id != profile.capacity_profile.profile_id
    {
        return Err(fatal(
            "OCOMP protocol bundle differs from the request profile",
        ));
    }
    bundle
        .validate_lysis_v1_input_codecs()
        .map_err(protocol_error)?;

    Ok(())
}

fn protocol_error(error: impl core::fmt::Display) -> PrecompileError {
    fatal(format!("invalid OCOMP activation authority: {error}"))
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}
