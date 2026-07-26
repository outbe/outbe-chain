use alloy_primitives::{Bytes, B256, U256};
use outbe_compressed_entities::ExecutionScope;
use outbe_intex::{install_certified_contributor_root, CertifiedContributorRootV1};
use outbe_lysis::activation_v1::{self, LysisApplyPlanV1, LysisOwnerReceiptsV1};
use outbe_nod::schema::NodContract;
use outbe_nodfactory::certified::{install_certified_generation, CertifiedNodGenerationV1};
use outbe_ocomp_protocol::{
    abi::{ACTIVATE_LYSIS_SELECTOR, OCOMP_ACTIVATION_REJECTED_SELECTOR},
    activation::{ActivationCallCoreV1, PoCActivationV1},
    committee::OcompCommitteeSnapshotV1,
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

use crate::precompile::IMetadosis;
use crate::schema::MetadosisContract;

use super::{
    schema::{poc_schema_limits, OcompRequestProfile},
    state::JobFsmLimits,
};

const REJECT_MALFORMED_ENCODING: u16 = 1;
const REJECT_LIMIT_EXCEEDED: u16 = 2;
const REJECT_FORK_OR_BUNDLE_MISMATCH: u16 = 3;
const REJECT_JOB_NOT_FOUND: u16 = 4;
const REJECT_JOB_TERMINAL: u16 = 5;
const REJECT_COMPLETED_BINDING_MISMATCH: u16 = 6;
const REJECT_DEADLINE_NOT_LIVE: u16 = 7;
const REJECT_FINALITY_PROOF_INVALID: u16 = 8;
const REJECT_JOB_BINDING_INVALID: u16 = 9;
const REJECT_COMMITTEE_SNAPSHOT_INVALID: u16 = 10;
const REJECT_CERTIFICATE_INVALID: u16 = 11;
const REJECT_RESULT_DIGEST_MISMATCH: u16 = 12;
const REJECT_RESULT_STRUCTURE_INVALID: u16 = 13;
const REJECT_BLOCK_ACTIVATION_LIMIT: u16 = 15;
const REJECT_OWNER_APPLY_REJECTED: u16 = 16;
const REJECT_RECEIPT_MISMATCH: u16 = 17;

/// Complete immutable consensus authority used by public LYSIS_V1 activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompActivationAuthorityV1 {
    pub bundle: ProtocolBundleV1,
    pub result_committee: OcompCommitteeSnapshotV1,
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

/// Returns the frozen typed rejection used when the block-scoped executor
/// meter has already admitted one public activation attempt.
pub fn reject_block_activation_limit() -> PrecompileError {
    reject(REJECT_BLOCK_ACTIVATION_LIMIT)
}

/// Dispatches the one frozen public activation method with current-block
/// execution scope and node-supplied finalized-intent authority.
pub fn dispatch_public_activation(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    finality_authority: Option<&dyn OcompFinalizedIntentAuthority>,
    data: &[u8],
    value: U256,
    is_static: bool,
) -> PrecompileResult<Bytes> {
    let limits = poc_schema_limits();
    if !value.is_zero() || is_static {
        return Err(reject(REJECT_MALFORMED_ENCODING));
    }
    let activation_bytes = preflight_activation_calldata(data)?;
    let activation = PoCActivationV1::decode_canonical(activation_bytes, &limits)
        .map_err(|error| protocol_reject(error, REJECT_MALFORMED_ENCODING))?;
    let current_height = storage.block_number()?;
    let current_time = storage
        .timestamp()?
        .try_into()
        .map_err(|_| fatal("OCOMP block timestamp does not fit u64"))?;

    let mut metadosis = MetadosisContract::new(storage.clone());
    let Some(record) = metadosis.ocomp_job_record(activation.intent_id, &limits)? else {
        return Err(reject(REJECT_JOB_NOT_FOUND));
    };
    if record.status == OcompJobStatus::Completed {
        return completed_retry(&activation, &record, &limits);
    }
    if record.status != OcompJobStatus::OffchainPending {
        return Err(reject(REJECT_JOB_TERMINAL));
    }
    if current_height >= record.intent.deadline_height {
        return Err(reject(REJECT_DEADLINE_NOT_LIVE));
    }

    let profile = metadosis
        .read_ocomp_request_profile(&limits)?
        .ok_or_else(|| fatal("pending OCOMP job has no request profile"))?;
    if record.intent.chain_id != profile.chain_id
        || record.intent.genesis_hash != profile.genesis_hash
        || record.intent.fork_id != profile.fork_id
        || record.intent.protocol_bundle_hash != profile.protocol_bundle_hash
    {
        return Err(reject(REJECT_FORK_OR_BUNDLE_MISMATCH));
    }
    let activation_authority = metadosis
        .read_ocomp_activation_authority(&limits)?
        .ok_or_else(|| fatal("pending OCOMP job has no activation authority"))?;
    if record.intent.result_committee_snapshot_hash
        != activation_authority
            .result_committee
            .snapshot_hash(&limits)
            .map_err(|error| protocol_reject(error, REJECT_COMMITTEE_SNAPSHOT_INVALID))?
    {
        return Err(reject(REJECT_COMMITTEE_SNAPSHOT_INVALID));
    }

    let finality_authority =
        finality_authority.ok_or_else(|| fatal("OCOMP finality authority is not installed"))?;
    let verified = finality_authority
        .verify(
            &activation.finalized_intent_proof,
            ExpectedFinalizedIntentBindingV1 {
                chain_id: profile.chain_id,
                genesis_hash: profile.genesis_hash,
                fork_id: profile.fork_id,
                protocol_bundle_hash: profile.protocol_bundle_hash,
            },
            &limits,
        )
        .map_err(finality_reject)?;
    validate_verified_job(&activation, &record, &verified, &limits)?;

    let reconstructed_payload = activation
        .result
        .activation_payload(&limits)
        .map_err(|error| protocol_reject(error, REJECT_RESULT_STRUCTURE_INVALID))?;
    if reconstructed_payload != activation.activation_payload {
        return Err(reject(REJECT_RESULT_DIGEST_MISMATCH));
    }
    let result_digest = reconstructed_payload
        .result_digest(&limits)
        .map_err(|error| protocol_reject(error, REJECT_RESULT_DIGEST_MISMATCH))?;
    if activation.certificate.result_digest != result_digest {
        return Err(reject(REJECT_RESULT_DIGEST_MISMATCH));
    }
    let result_evidence_hash = activation
        .result_evidence_hash(&limits)
        .map_err(|error| protocol_reject(error, REJECT_RESULT_STRUCTURE_INVALID))?;
    activation
        .certificate
        .verify(
            &activation_authority.result_committee,
            current_height,
            &limits,
        )
        .map_err(|_| reject(REJECT_CERTIFICATE_INVALID))?;
    activation
        .verify_structure(
            verified.request.state_root,
            &activation_authority.result_committee,
            &limits,
        )
        .map_err(|error| {
            fatal(format!(
                "OCOMP staged verification disagrees with canonical verifier: {error}"
            ))
        })?;

    let plan = activation_v1::verify_result(
        activation.intent_id,
        verified.job_id,
        &record.intent,
        &activation.activation_payload,
        &activation.result,
        &limits,
    )
    .map_err(|error| protocol_reject(error, REJECT_RESULT_STRUCTURE_INVALID))?;

    let ce_checkpoint = scope.ce_work_checkpoint()?;
    let outcome = storage.with_checkpoint(|| {
        let job_fsm_limits = fsm_limits(&profile);
        if target_preconditions_changed(&storage, &metadosis, &record, &limits, job_fsm_limits)? {
            commit_conflict(
                &mut metadosis,
                &plan,
                result_evidence_hash,
                current_height,
                current_time,
                &limits,
                job_fsm_limits,
            )
        } else {
            apply_certified_result(
                &storage,
                scope,
                &mut metadosis,
                &activation,
                &activation_authority.bundle,
                &plan,
                result_evidence_hash,
                current_height,
                current_time,
                &limits,
                job_fsm_limits,
            )
        }
    });
    if outcome.is_err() {
        scope.restore_ce_work_checkpoint(ce_checkpoint)?;
    }
    outcome
}

fn preflight_activation_calldata(data: &[u8]) -> PrecompileResult<&[u8]> {
    let candidate = outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1;
    let calldata_cap = usize::try_from(candidate.max_activation_calldata_bytes)
        .map_err(|_| fatal("OCOMP activation calldata cap does not fit usize"))?;
    if data.len() > calldata_cap {
        return Err(reject(REJECT_LIMIT_EXCEEDED));
    }
    if data.len() < 68
        || data.get(..4) != Some(ACTIVATE_LYSIS_SELECTOR.as_slice())
        || U256::from_be_slice(&data[4..36]) != U256::from(32)
    {
        return Err(reject(REJECT_MALFORMED_ENCODING));
    }
    let payload_len_word = U256::from_be_slice(&data[36..68]);
    let payload_len =
        usize::try_from(payload_len_word).map_err(|_| reject(REJECT_LIMIT_EXCEEDED))?;
    let payload_cap = usize::try_from(candidate.max_activation_ocb1_bytes)
        .map_err(|_| fatal("OCOMP activation object cap does not fit usize"))?;
    if payload_len == 0 || payload_len > payload_cap {
        return Err(reject(REJECT_LIMIT_EXCEEDED));
    }
    let padded_len = payload_len
        .checked_add(31)
        .map(|value| value & !31)
        .ok_or_else(|| reject(REJECT_LIMIT_EXCEEDED))?;
    let expected_len = 68_usize
        .checked_add(padded_len)
        .ok_or_else(|| reject(REJECT_LIMIT_EXCEEDED))?;
    if data.len() != expected_len {
        return Err(reject(REJECT_MALFORMED_ENCODING));
    }
    let payload_end = 68 + payload_len;
    if data[payload_end..].iter().any(|byte| *byte != 0) {
        return Err(reject(REJECT_MALFORMED_ENCODING));
    }
    Ok(&data[68..payload_end])
}

fn completed_retry(
    activation: &PoCActivationV1,
    record: &OcompJobRecordV1,
    limits: &SchemaLimits,
) -> PrecompileResult<Bytes> {
    let Some(terminal) = &record.terminal else {
        return Err(fatal("completed OCOMP job has no terminal record"));
    };
    let Some(completed) = &terminal.completed_binding else {
        return Err(fatal("completed OCOMP job has no completed binding"));
    };
    let claimed_payload = activation
        .result
        .activation_payload(limits)
        .map_err(|_| reject(REJECT_COMPLETED_BINDING_MISMATCH))?;
    if claimed_payload != activation.activation_payload {
        return Err(reject(REJECT_COMPLETED_BINDING_MISMATCH));
    }
    let claimed_digest = claimed_payload
        .result_digest(limits)
        .map_err(|_| reject(REJECT_COMPLETED_BINDING_MISMATCH))?;
    let claimed_evidence_hash = activation
        .result_evidence_hash(limits)
        .map_err(|_| reject(REJECT_COMPLETED_BINDING_MISMATCH))?;
    let activation_preconditions_hash = record
        .intent
        .activation_preconditions
        .activation_preconditions_hash(limits)
        .map_err(|error| fatal(format!("hash completed activation preconditions: {error}")))?;
    let claimed_call = ActivationCallCoreV1 {
        intent_id: activation.intent_id,
        job_id: activation.result.job_id,
        attempt: activation.result.attempt,
        protocol_bundle_hash: activation.result.protocol_bundle_hash,
        result_digest: claimed_digest,
        activation_preconditions_hash,
        terminal_pending_nonce: record.intent.pending_nonce,
    };
    let claimed_call_id = claimed_call
        .activation_call_id(limits)
        .map_err(|_| reject(REJECT_COMPLETED_BINDING_MISMATCH))?;
    if completed.job_id != activation.result.job_id
        || completed.activation_call_id != claimed_call_id
        || completed.result_digest != claimed_digest
        || completed.result_evidence_hash != claimed_evidence_hash
        || completed.terminal_receipt.outcome != ActivationOutcome::Applied
    {
        return Err(reject(REJECT_COMPLETED_BINDING_MISMATCH));
    }
    Ok(encode_activation_return(
        completed.activation_call_id,
        completed.result_digest,
        ActivationOutcome::Applied,
    ))
}

fn validate_verified_job(
    activation: &PoCActivationV1,
    record: &OcompJobRecordV1,
    verified: &VerifiedFinalizedIntentV1,
    limits: &SchemaLimits,
) -> PrecompileResult<()> {
    let record_intent_id = record
        .intent
        .intent_id(limits)
        .map_err(|error| fatal(format!("hash live OCOMP intent: {error}")))?;
    if activation.intent_id != record_intent_id
        || verified.intent_id != record_intent_id
        || verified.intent != record.intent
        || activation.result.job_id != verified.job_id
        || activation.result.attempt != record.intent.attempt
        || activation.result.protocol_bundle_hash != record.intent.protocol_bundle_hash
    {
        return Err(reject(REJECT_JOB_BINDING_INVALID));
    }
    activation
        .result
        .validate_finalized_intent(&record.intent)
        .map_err(|error| protocol_reject(error, REJECT_JOB_BINDING_INVALID))
}

fn target_preconditions_changed(
    storage: &StorageHandle<'_>,
    metadosis: &MetadosisContract<'_>,
    record: &OcompJobRecordV1,
    limits: &SchemaLimits,
    fsm_limits: JobFsmLimits,
) -> PrecompileResult<bool> {
    let expected = &record.intent.activation_preconditions;
    let wwd = outbe_common::WorldwideDay::new(record.intent.wwd);
    let tribute = TributeContract::new(storage.clone()).pre_admission_projection(wwd)?;
    let nod = NodContract::new(storage.clone()).ocomp_target_projection(wwd)?;
    let contributors = outbe_intex::api::ocomp_contributor_target_projection(
        storage,
        expected.contributors.series_id,
    )?;
    let metadosis_projection = metadosis.ocomp_pre_admission_projection(wwd)?;
    let fsm = metadosis
        .live_ocomp_fsm_state(limits, fsm_limits)?
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
        || status != crate::schema::status::OFFCHAIN_PENDING
        || fsm.live_intent_id
            != Some(
                record
                    .intent
                    .intent_id(limits)
                    .map_err(|error| fatal(format!("hash target OCOMP intent: {error}")))?,
            )
        || fsm.pending_nonce != expected.metadosis.pending_nonce)
}

fn commit_conflict(
    metadosis: &mut MetadosisContract<'_>,
    plan: &LysisApplyPlanV1,
    result_evidence_hash: B256,
    current_height: u64,
    current_time: u64,
    limits: &SchemaLimits,
    fsm_limits: JobFsmLimits,
) -> PrecompileResult<Bytes> {
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
        result_evidence_hash,
        terminal_receipt_hash,
        terminal_receipt: receipt,
    };
    let next_pending_nonce = metadosis.commit_ocomp_conflict(
        binding.intent_id,
        completed_binding,
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

#[allow(clippy::too_many_arguments)]
fn apply_certified_result(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    metadosis: &mut MetadosisContract<'_>,
    activation: &PoCActivationV1,
    bundle: &ProtocolBundleV1,
    plan: &LysisApplyPlanV1,
    result_evidence_hash: B256,
    current_height: u64,
    current_time: u64,
    limits: &SchemaLimits,
    fsm_limits: JobFsmLimits,
) -> PrecompileResult<Bytes> {
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
        roots: activation.result.roots.clone(),
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
            binding.intent_id,
            active_generation,
            result_evidence_hash,
            plan.nod().nod_gratis_consumed(),
            plan.carry_over().credited_unused_lysis(),
            current_height,
            current_time,
            permit,
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
    #[cfg(feature = "test-utils")]
    super::test_support::inject_receipt_fault(request_receipt, receipts);
    #[cfg(not(feature = "test-utils"))]
    {
        let _ = (request_receipt, receipts);
    }
}

fn owner_apply_error(error: PrecompileError) -> PrecompileError {
    match error {
        PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => {
            reject(REJECT_OWNER_APPLY_REJECTED)
        }
        other => other,
    }
}

fn fsm_limits(profile: &OcompRequestProfile) -> JobFsmLimits {
    JobFsmLimits {
        max_terminal_records: profile.capacity_profile.max_terminal_job_records,
    }
}

fn finality_reject(error: OcompFinalityAuthorityError) -> PrecompileError {
    match error {
        OcompFinalityAuthorityError::LocalAuthority(message) => {
            fatal(format!("OCOMP finalized authority failure: {message}"))
        }
        OcompFinalityAuthorityError::InvalidProof(error) => match error {
            FinalizedIntentVerificationError::WrongChain
            | FinalizedIntentVerificationError::WrongGenesis
            | FinalizedIntentVerificationError::WrongFork
            | FinalizedIntentVerificationError::WrongProtocolBundle => {
                reject(REJECT_FORK_OR_BUNDLE_MISMATCH)
            }
            _ => reject(REJECT_FINALITY_PROOF_INVALID),
        },
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
    /// Installs the complete bundle and result committee exactly once.
    ///
    /// Their hashes must already be pinned by the immutable request profile.
    /// Repeating the exact pair is idempotent; partial or replacement state is
    /// fatal because it would make request and activation authority diverge.
    pub fn initialize_ocomp_activation_authority(
        &mut self,
        bundle: &ProtocolBundleV1,
        result_committee: &OcompCommitteeSnapshotV1,
        limits: &SchemaLimits,
    ) -> PrecompileResult<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let profile = self
                .read_ocomp_request_profile(limits)?
                .ok_or_else(|| fatal("OCOMP request profile is not initialized"))?;
            validate_activation_authority(&profile, bundle, result_committee, limits)?;

            match self.read_ocomp_activation_authority(limits)? {
                Some(existing)
                    if existing.bundle == *bundle
                        && existing.result_committee == *result_committee =>
                {
                    Ok(())
                }
                Some(_) => Err(fatal("OCOMP activation authority is immutable")),
                None => {
                    self.ocomp_active_protocol_bundle
                        .write(&bundle.encode_canonical(limits).map_err(protocol_error)?)?;
                    self.ocomp_result_committee_snapshot.write(
                        &result_committee
                            .encode_canonical(limits)
                            .map_err(protocol_error)?,
                    )?;
                    if self.read_ocomp_activation_authority(limits)?
                        != Some(OcompActivationAuthorityV1 {
                            bundle: bundle.clone(),
                            result_committee: result_committee.clone(),
                        })
                    {
                        return Err(fatal("OCOMP activation authority write/read mismatch"));
                    }
                    Ok(())
                }
            }
        })
    }

    pub fn read_ocomp_activation_authority(
        &self,
        limits: &SchemaLimits,
    ) -> PrecompileResult<Option<OcompActivationAuthorityV1>> {
        let bundle_len = self.ocomp_active_protocol_bundle.len()?;
        let committee_len = self.ocomp_result_committee_snapshot.len()?;
        match (bundle_len, committee_len) {
            (0, 0) => return Ok(None),
            (0, _) | (_, 0) => return Err(fatal("OCOMP activation authority is partial")),
            _ => {}
        }
        let max = limits
            .codec
            .max_body_bytes
            .checked_add(OCB1_HEADER_LEN)
            .ok_or_else(|| fatal("OCOMP activation authority byte cap overflow"))?;
        if bundle_len > max || committee_len > max {
            return Err(fatal("OCOMP activation authority exceeds byte cap"));
        }
        let bundle =
            ProtocolBundleV1::decode_canonical(&self.ocomp_active_protocol_bundle.read()?, limits)
                .map_err(protocol_error)?;
        let result_committee = OcompCommitteeSnapshotV1::decode_canonical(
            &self.ocomp_result_committee_snapshot.read()?,
            limits,
        )
        .map_err(protocol_error)?;
        let profile = self
            .read_ocomp_request_profile(limits)?
            .ok_or_else(|| fatal("OCOMP activation authority has no request profile"))?;
        validate_activation_authority(&profile, &bundle, &result_committee, limits)?;
        Ok(Some(OcompActivationAuthorityV1 {
            bundle,
            result_committee,
        }))
    }
}

fn validate_activation_authority(
    profile: &OcompRequestProfile,
    bundle: &ProtocolBundleV1,
    result_committee: &OcompCommitteeSnapshotV1,
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

    let committee_hash = result_committee
        .snapshot_hash(limits)
        .map_err(protocol_error)?;
    if result_committee.chain_id != profile.chain_id
        || result_committee.genesis_hash != profile.genesis_hash
        || result_committee.fork_id != profile.fork_id
        || result_committee.protocol_bundle_hash != profile.protocol_bundle_hash
        || committee_hash != profile.result_committee_snapshot_hash
    {
        return Err(fatal(
            "OCOMP result committee differs from the request profile",
        ));
    }
    Ok(())
}

fn protocol_error(error: impl core::fmt::Display) -> PrecompileError {
    fatal(format!("invalid OCOMP activation authority: {error}"))
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calldata(payload: &[u8]) -> Vec<u8> {
        let padded_len = (payload.len() + 31) & !31;
        let mut encoded = Vec::with_capacity(68 + padded_len);
        encoded.extend_from_slice(&ACTIVATE_LYSIS_SELECTOR);
        encoded.extend_from_slice(&U256::from(32).to_be_bytes::<32>());
        encoded.extend_from_slice(&U256::from(payload.len()).to_be_bytes::<32>());
        encoded.extend_from_slice(payload);
        encoded.resize(68 + padded_len, 0);
        encoded
    }

    fn rejection_code(error: PrecompileError) -> u16 {
        let PrecompileError::RevertBytes(bytes) = error else {
            panic!("expected typed OCOMP rejection");
        };
        assert_eq!(&bytes[..4], &OCOMP_ACTIVATION_REJECTED_SELECTOR);
        u16::try_from(U256::from_be_slice(&bytes[4..36])).unwrap()
    }

    #[test]
    fn bounded_abi_preflight_accepts_only_exact_canonical_dynamic_bytes() {
        let exact = calldata(&[1, 2, 3]);
        assert_eq!(preflight_activation_calldata(&exact).unwrap(), &[1, 2, 3]);

        let mut nonzero_padding = exact.clone();
        *nonzero_padding.last_mut().unwrap() = 1;
        assert_eq!(
            rejection_code(preflight_activation_calldata(&nonzero_padding).unwrap_err()),
            REJECT_MALFORMED_ENCODING
        );

        let mut wrong_offset = exact.clone();
        wrong_offset[35] = 31;
        assert_eq!(
            rejection_code(preflight_activation_calldata(&wrong_offset).unwrap_err()),
            REJECT_MALFORMED_ENCODING
        );

        let mut trailing = exact;
        trailing.push(0);
        assert_eq!(
            rejection_code(preflight_activation_calldata(&trailing).unwrap_err()),
            REJECT_MALFORMED_ENCODING
        );
    }

    #[test]
    fn bounded_abi_preflight_rejects_zero_and_over_cap_payloads_before_decode() {
        assert_eq!(
            rejection_code(preflight_activation_calldata(&calldata(&[])).unwrap_err()),
            REJECT_LIMIT_EXCEEDED
        );

        let cap = usize::try_from(
            outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1
                .max_activation_ocb1_bytes,
        )
        .unwrap();
        let over_cap = calldata(&vec![0; cap + 1]);
        assert_eq!(
            rejection_code(preflight_activation_calldata(&over_cap).unwrap_err()),
            REJECT_LIMIT_EXCEEDED
        );
    }

    #[test]
    fn block_meter_rejection_uses_frozen_code_fifteen() {
        assert_eq!(
            rejection_code(reject_block_activation_limit()),
            REJECT_BLOCK_ACTIVATION_LIMIT
        );
    }
}
