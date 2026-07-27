//! Mutable fixture state threaded across a scenario's steps.
//!
//! Values a step computes and a later step reads back (the proposal under test,
//! the version/heights we proposed, the deadline we observed). Kept off the
//! handles so `localnet`/`rpc`/`validators` stay stateless verbs.

use serde::Serialize;

/// Exact public-chain measurements for the q-forming S+1 capacity block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompPublicCapacityObservationV1 {
    pub job_id: alloy_primitives::B256,
    pub result_digest: alloy_primitives::B256,
    pub q_forming_transaction_hash: alloy_primitives::B256,
    pub q_forming_block_number: u64,
    pub q_forming_block_hash: alloy_primitives::B256,
    pub finalized_block_number: u64,
    pub finalized_block_hash: alloy_primitives::B256,
    pub tribute_count: u64,
    pub nod_count: u64,
    pub worker_shard_count: u64,
    pub transaction_bytes: u64,
    pub block_bytes: u64,
    pub gas: u64,
    pub internal_work: u64,
    pub block_processing_micros_by_validator: Vec<u64>,
    pub block_processing_micros: u64,
    pub finality_latency_micros: u64,
}

/// Public outcome recovered after one validator discards only its derived CE
/// database and rebuilds it from preserved canonical Reth history.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompHistoricalReplayObservationV1 {
    pub recovery: crate::world::localnet::CeStartupReplayObservationV1,
    pub recovered_result_digest: alloy_primitives::B256,
    pub recovered_generation: crate::world::rpc::OcompCertifiedGenerationV1,
}

/// Public-path observations retained after behavioral assertions complete.
///
/// This is evidence, not a control surface: every field is populated from
/// finalized public RPC data or from the result of a transaction exercised by
/// a Cucumber step.
#[derive(Clone, Debug, Default, Serialize)]
pub struct OcompPublicScenarioEvidenceV1 {
    pub job_request: Option<crate::world::rpc::OcompPublicJobRequestV1>,
    pub activation: Option<crate::world::rpc::OcompPublicActivationV1>,
    pub certified_generation: Option<crate::world::rpc::OcompCertifiedGenerationV1>,
    pub result_vote_transactions: Vec<crate::world::rpc::OcompPublicResultVoteTransactionV1>,
    pub vote_accountability: Option<crate::world::rpc::OcompPublicVoteAccountabilityV1>,
    pub validator_balances_before: Vec<(alloy_primitives::Address, alloy_primitives::U256)>,
    pub validator_balances_after: Vec<(alloy_primitives::Address, alloy_primitives::U256)>,
    pub atomic_quorum_apply_verified: bool,
    pub exact_completed_retry_succeeded: Option<bool>,
    pub changed_completed_binding_reverted: Option<bool>,
    pub completed_state_unchanged: Option<bool>,
    pub non_quorum_changed_binding_reverted: Option<bool>,
    pub non_quorum_state_unchanged: Option<bool>,
    pub expired_without_nod: Option<bool>,
    pub held_late_vote_hash: Option<alloy_primitives::B256>,
    pub late_vote_reverted: Option<bool>,
    pub late_vote_inclusion_height: Option<u64>,
    pub capacity_resources: Option<crate::ocomp_capacity::OcompCapacityResourceObservationV1>,
    pub capacity_public_path: Option<OcompPublicCapacityObservationV1>,
    pub capacity_historical_replay: Option<OcompHistoricalReplayObservationV1>,
}

/// Per-scenario state accumulated as the steps run.
#[derive(Debug)]
pub struct FixtureState {
    /// Proposal id under test (always 1 in the update flow).
    pub proposal_id: u64,
    /// The protocol version we proposed (active + 1).
    pub proposed_version: Option<u64>,
    /// The activation height carried in the proposal payload.
    pub activation_height: Option<u64>,
    /// The vote deadline height read back from `vote status`.
    pub vote_deadline: Option<u64>,
    /// Voting window (blocks) the localnet was started with.
    pub voting_window: u64,
    /// The unsupported-update scenario deliberately emits one narrowly matched
    /// fatal compatibility message; every other fatal/alarm remains forbidden.
    pub allow_unsupported_update_fatal: bool,
    /// One stalled-reshare scenario deliberately leaves this exact participant
    /// offline long enough for the protocol's documented share-reveal path.
    /// Every other reveal/fatal/alarm remains forbidden by the log audit.
    pub expected_dkg_reveal: Option<String>,

    // ---- validator-lifecycle scenarios (s1..s7 / follower) ----
    /// Provisioned joiner's EOA address (derived after `provision`).
    pub joiner_addr: Option<String>,
    /// The chain's worldwide-day key used for tribute offers.
    pub wwd: Option<String>,
    /// A height captured by one step for a later assertion (kill/restart/exit).
    pub marker_height: Option<u64>,
    /// A log-line count captured before an action (e.g. DKG ceremony count).
    pub marker_count: Option<usize>,
    /// VRF expiry observed while a permanently stalled frozen DKG target is live.
    pub vrf_expiry_height: Option<u64>,
    /// Worldwide-day status byte captured before a tribute offer (invariant check).
    pub wwd_status_before: Option<String>,
    /// Exact lifecycle accounting snapshot captured immediately before exit.
    pub lifecycle_stake_before_exit: Option<alloy_primitives::U256>,
    pub lifecycle_total_before_exit: Option<alloy_primitives::U256>,
    pub lifecycle_staking_balance_before_exit: Option<alloy_primitives::U256>,
    /// Exact punitive-accounting snapshot captured before a validator is taken offline.
    pub slash_stake_before: Option<alloy_primitives::U256>,
    pub slash_total_before: Option<alloy_primitives::U256>,
    pub slash_staking_balance_before: Option<alloy_primitives::U256>,
    pub slash_count_before: Option<u64>,
    /// Stake remaining immediately after the first felony, used to prove idempotency.
    pub slash_stake_after: Option<alloy_primitives::U256>,
    /// Hash of the encrypted tribute transaction under projection verification.
    pub tribute_tx_hash: Option<String>,
    /// Private keys of deterministic genesis-funded owners used only by the
    /// OCM-26 maximum-shaped public capacity fixture. They are never emitted
    /// into scenario evidence.
    pub ocomp_capacity_tribute_private_keys: Vec<String>,
    /// Public transaction hashes submitted by those capacity owners.
    pub ocomp_capacity_tribute_tx_hashes: Vec<String>,
    /// Hash of a duplicate logical offer expected to be rejected without state changes.
    pub duplicate_tribute_tx_hash: Option<String>,
    /// Exact primary/owner/day Mongo documents before a duplicate offer.
    pub tribute_projection_before_duplicate:
        Option<crate::world::mongodb::TributeProjectionSnapshot>,
    /// Finalized height immediately before one typed OCOMP process fault.
    pub ocomp_finality_before_fault: Option<u64>,
    /// Public, finalized Metadosis request observed identically on all four
    /// validators. This is evidence only; the harness cannot create the job.
    pub ocomp_job_request: Option<crate::world::rpc::OcompPublicJobRequestV1>,
    /// Public activation event observed identically on all validators.
    pub ocomp_activation: Option<crate::world::rpc::OcompPublicActivationV1>,
    /// Cross-owner proof authority read at the exact finalized activation block.
    pub ocomp_certified_generation: Option<crate::world::rpc::OcompCertifiedGenerationV1>,
    /// Four public result-vote transactions observed from finalized block and
    /// receipt RPC data, never from Supervisor-local journals.
    pub ocomp_result_vote_transactions: Vec<crate::world::rpc::OcompPublicResultVoteTransactionV1>,
    /// Finalized four-slot accountability state observed identically on every
    /// validator after the late fourth vote.
    pub ocomp_vote_accountability: Option<crate::world::rpc::OcompPublicVoteAccountabilityV1>,
    /// Validator EVM balances captured before public result-vote submission.
    pub ocomp_validator_balances_before: Vec<(alloy_primitives::Address, alloy_primitives::U256)>,
    /// Same validator balances after all four ZeroFee result votes.
    pub ocomp_validator_balances_after: Vec<(alloy_primitives::Address, alloy_primitives::U256)>,
    /// Behavioral public-path outcomes retained for independent evidence
    /// aggregation after the scenario.
    pub ocomp_atomic_quorum_apply_verified: bool,
    pub ocomp_exact_completed_retry_succeeded: Option<bool>,
    pub ocomp_changed_completed_binding_reverted: Option<bool>,
    pub ocomp_completed_state_unchanged: Option<bool>,
    pub ocomp_non_quorum_changed_binding_reverted: Option<bool>,
    pub ocomp_non_quorum_state_unchanged: Option<bool>,
    pub ocomp_expired_without_nod: Option<bool>,
    /// A node-signed transaction held by the deadline scenario until the
    /// exclusive boundary. Raw bytes are never published as scenario evidence.
    pub ocomp_held_late_vote_raw: Option<Vec<u8>>,
    pub ocomp_held_late_vote_hash: Option<alloy_primitives::B256>,
    pub ocomp_late_vote_reverted: Option<bool>,
    pub ocomp_late_vote_inclusion_height: Option<u64>,
    pub ocomp_capacity_observation: Option<OcompPublicCapacityObservationV1>,
    pub ocomp_historical_replay_observation: Option<OcompHistoricalReplayObservationV1>,

    // ---- L2Registry zk-gate scenarios (PFS-001-10 / -11) ----
    /// Encoded BLS MinPk private key the harness registered as the L2 network key.
    pub l2_bls_private_hex: Option<String>,
    /// L2 chain id registered for the operator under test.
    pub l2_chain_id: Option<u64>,
    /// Hash of an offer expected to be rejected by the zk signature gate.
    pub l2_rejected_offer_tx_hash: Option<String>,

    // ---- ZeroFee live scenario ----
    pub zerofee_key: Option<String>,
    pub zerofee_address: Option<String>,
    pub zerofee_delegation_receipt: Option<serde_json::Value>,
    pub zerofee_sponsored_receipts: Vec<serde_json::Value>,
    /// Exact signed raw transaction for one already included sponsored call.
    pub zerofee_sponsored_raw: Option<String>,
    pub zerofee_balance_before: Option<alloy_primitives::U256>,
    pub zerofee_balance_after_quota: Option<alloy_primitives::U256>,
    pub zerofee_ninth_receipt: Option<serde_json::Value>,
    pub zerofee_balance_after_ninth: Option<alloy_primitives::U256>,
    pub zerofee_paid_receipt: Option<serde_json::Value>,
    pub zerofee_balance_after_paid: Option<alloy_primitives::U256>,
    /// Exact signed EIP-7702 transaction returned by public RPC for replay.
    pub zerofee_delegation_raw: Option<String>,
    /// RPC rejection observed when the exact signed transaction is replayed.
    pub zerofee_replay_error: Option<String>,
    pub zerofee_negative_key: Option<String>,
    pub zerofee_negative_address: Option<String>,
    pub zerofee_invalid_authorization_receipt: Option<serde_json::Value>,
    pub zerofee_wrong_target_receipt: Option<serde_json::Value>,
    pub zerofee_wrong_target_balance_before: Option<alloy_primitives::U256>,
    pub zerofee_wrong_target_balance_after: Option<alloy_primitives::U256>,
    pub zerofee_conflicting_authorization_receipt: Option<serde_json::Value>,
    pub zerofee_day_before_rollover: Option<u32>,
    pub zerofee_new_day_receipt: Option<serde_json::Value>,
    pub zerofee_new_day_balance_before: Option<alloy_primitives::U256>,
    pub zerofee_new_day_balance_after: Option<alloy_primitives::U256>,
}

impl Default for FixtureState {
    fn default() -> Self {
        Self {
            proposal_id: 1,
            proposed_version: None,
            activation_height: None,
            vote_deadline: None,
            voting_window: 6,
            allow_unsupported_update_fatal: false,
            expected_dkg_reveal: None,
            joiner_addr: None,
            wwd: None,
            marker_height: None,
            marker_count: None,
            vrf_expiry_height: None,
            wwd_status_before: None,
            lifecycle_stake_before_exit: None,
            lifecycle_total_before_exit: None,
            lifecycle_staking_balance_before_exit: None,
            slash_stake_before: None,
            slash_total_before: None,
            slash_staking_balance_before: None,
            slash_count_before: None,
            slash_stake_after: None,
            tribute_tx_hash: None,
            ocomp_capacity_tribute_private_keys: Vec::new(),
            ocomp_capacity_tribute_tx_hashes: Vec::new(),
            duplicate_tribute_tx_hash: None,
            tribute_projection_before_duplicate: None,
            ocomp_finality_before_fault: None,
            ocomp_job_request: None,
            ocomp_activation: None,
            ocomp_certified_generation: None,
            ocomp_result_vote_transactions: Vec::new(),
            ocomp_vote_accountability: None,
            ocomp_validator_balances_before: Vec::new(),
            ocomp_validator_balances_after: Vec::new(),
            ocomp_atomic_quorum_apply_verified: false,
            ocomp_exact_completed_retry_succeeded: None,
            ocomp_changed_completed_binding_reverted: None,
            ocomp_completed_state_unchanged: None,
            ocomp_non_quorum_changed_binding_reverted: None,
            ocomp_non_quorum_state_unchanged: None,
            ocomp_expired_without_nod: None,
            ocomp_held_late_vote_raw: None,
            ocomp_held_late_vote_hash: None,
            ocomp_late_vote_reverted: None,
            ocomp_late_vote_inclusion_height: None,
            ocomp_capacity_observation: None,
            ocomp_historical_replay_observation: None,
            l2_bls_private_hex: None,
            l2_chain_id: None,
            l2_rejected_offer_tx_hash: None,
            zerofee_key: None,
            zerofee_address: None,
            zerofee_delegation_receipt: None,
            zerofee_sponsored_receipts: Vec::new(),
            zerofee_sponsored_raw: None,
            zerofee_balance_before: None,
            zerofee_balance_after_quota: None,
            zerofee_ninth_receipt: None,
            zerofee_balance_after_ninth: None,
            zerofee_paid_receipt: None,
            zerofee_balance_after_paid: None,
            zerofee_delegation_raw: None,
            zerofee_replay_error: None,
            zerofee_negative_key: None,
            zerofee_negative_address: None,
            zerofee_invalid_authorization_receipt: None,
            zerofee_wrong_target_receipt: None,
            zerofee_wrong_target_balance_before: None,
            zerofee_wrong_target_balance_after: None,
            zerofee_conflicting_authorization_receipt: None,
            zerofee_day_before_rollover: None,
            zerofee_new_day_receipt: None,
            zerofee_new_day_balance_before: None,
            zerofee_new_day_balance_after: None,
        }
    }
}

impl FixtureState {
    #[must_use]
    pub fn ocomp_public_scenario_evidence(&self) -> OcompPublicScenarioEvidenceV1 {
        OcompPublicScenarioEvidenceV1 {
            job_request: self.ocomp_job_request.clone(),
            activation: self.ocomp_activation.clone(),
            certified_generation: self.ocomp_certified_generation.clone(),
            result_vote_transactions: self.ocomp_result_vote_transactions.clone(),
            vote_accountability: self.ocomp_vote_accountability.clone(),
            validator_balances_before: self.ocomp_validator_balances_before.clone(),
            validator_balances_after: self.ocomp_validator_balances_after.clone(),
            atomic_quorum_apply_verified: self.ocomp_atomic_quorum_apply_verified,
            exact_completed_retry_succeeded: self.ocomp_exact_completed_retry_succeeded,
            changed_completed_binding_reverted: self.ocomp_changed_completed_binding_reverted,
            completed_state_unchanged: self.ocomp_completed_state_unchanged,
            non_quorum_changed_binding_reverted: self.ocomp_non_quorum_changed_binding_reverted,
            non_quorum_state_unchanged: self.ocomp_non_quorum_state_unchanged,
            expired_without_nod: self.ocomp_expired_without_nod,
            held_late_vote_hash: self.ocomp_held_late_vote_hash,
            late_vote_reverted: self.ocomp_late_vote_reverted,
            late_vote_inclusion_height: self.ocomp_late_vote_inclusion_height,
            capacity_resources: None,
            capacity_public_path: self.ocomp_capacity_observation.clone(),
            capacity_historical_replay: self.ocomp_historical_replay_observation.clone(),
        }
    }
}
