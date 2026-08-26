//! Mutable fixture state threaded across a scenario's steps.
//!
//! Values a step computes and a later step reads back (the proposal under test,
//! the version/heights we proposed, the deadline we observed). Kept off the
//! handles so `localnet`/`rpc`/`validators` stay stateless verbs.

use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
pub struct RadicleScenarioEvidenceV1 {
    pub genesis_hash: Option<String>,
    pub repo_id: Option<String>,
    pub repo_id_hex: Option<String>,
    pub registration_transaction: Option<String>,
    pub registration_finalized_height: Option<u64>,
    pub issue_id: Option<String>,
    pub patch_id: Option<String>,
    pub pushed_commit: Option<String>,
    pub source_home: Option<std::path::PathBuf>,
    pub source_repository: Option<std::path::PathBuf>,
    pub validator_node_ids: Vec<String>,
    pub validator_native_node_ids: Vec<String>,
    pub founder_validator_addresses: Vec<String>,
    pub founder_onchain_node_ids: Vec<String>,
    pub signed_endpoint_frames: Vec<Vec<serde_json::Value>>,
    pub initial_native_session_sets: Vec<Vec<String>>,
    pub initial_seed_scope_all_validators: Vec<usize>,
    pub validator_sidecar_pids_before: Vec<u32>,
    pub endpoint_node_pid_before: Option<u32>,
    pub endpoint_node_pid_after: Option<u32>,
    pub endpoint_sidecar_pid_before: Option<u32>,
    pub endpoint_sidecar_pid_after: Option<u32>,
    pub endpoint_old_port: Option<u16>,
    pub endpoint_new_port: Option<u16>,
    pub endpoint_replacement_signed_frames: Vec<serde_json::Value>,
    pub endpoint_old_session_addresses: Vec<String>,
    pub endpoint_new_session_addresses: Vec<String>,
    pub endpoint_replacement_session_sets: Vec<Vec<String>>,
    pub sidecar_fault_pid_before: Option<u32>,
    pub sidecar_recovery_pid_after: Option<u32>,
    pub finality_before_sidecar_fault: Option<u64>,
    pub finality_after_sidecar_fault: Option<u64>,
    pub sidecar_recovery_session_sets: Vec<Vec<String>>,
    pub sidecar_recovery_seed_scope_all: Option<bool>,
    pub node_restart_pid_before: Option<u32>,
    pub node_restart_pid_after: Option<u32>,
    pub node_restart_sidecar_pid_before: Option<u32>,
    pub node_restart_sidecar_pid_after: Option<u32>,
    pub finality_before_node_restart: Option<u64>,
    pub finality_after_node_restart: Option<u64>,
    pub node_recovery_session_sets: Vec<Vec<String>>,
    pub node_recovery_seed_scope_all: Option<bool>,
    pub joiner_node_id: Option<String>,
    pub joiner_activation_finalized_height: Option<u64>,
    pub final_native_session_sets: Vec<Vec<String>>,
    pub final_seed_scope_all_validators: Vec<usize>,
}

/// Exact public-chain measurements for the q-forming S+1 capacity block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompPublicCapacityObservationV1 {
    pub job_id: alloy_primitives::B256,
    pub result_digest: alloy_primitives::B256,
    pub q_forming_transaction_hash: alloy_primitives::B256,
    pub q_forming_block_number: u64,
    pub q_forming_block_hash: alloy_primitives::B256,
    pub q_forming_receipt_success: bool,
    pub q_forming_receipt_sha256: String,
    pub q_forming_validator_receipt_sha256: Vec<String>,
    pub q_forming_state_root: alloy_primitives::B256,
    pub q_forming_ce_root: alloy_primitives::B256,
    pub q_forming_validator_commitments: Vec<crate::world::rpc::BlockCommitmentV1>,
    pub canonical_import_validator_count: u8,
    pub canonical_import_verified: bool,
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

/// One canonical finalized point observed by a validator around a controlled
/// testnet logical-time restart.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisFinalizedPointV1 {
    pub validator_index: u8,
    pub block_number: u64,
    pub block_hash: alloy_primitives::B256,
    pub block_timestamp: u64,
}

/// Exact evidence for one committee-wide logical-time epoch. The restart changes
/// only the existing testnet time source; genesis, datadirs and chain history
/// remain continuous.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisTimeControlEpochV1 {
    pub requested_timestamp: u64,
    pub unix_time_offset_secs: i64,
    pub before_restart: Vec<MetadosisFinalizedPointV1>,
    pub after_restart: Vec<MetadosisFinalizedPointV1>,
}

/// Same-chain lifecycle evidence for the fresh Metadosis process lane.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisFreshLifecycleObservationV1 {
    pub worldwide_day: u32,
    pub genesis_hash: alloy_primitives::B256,
    pub initial_timestamp: u64,
    pub initial_unix_time_offset_secs: i64,
    pub forming_start: u64,
    pub forming_end: u64,
    pub lookback_end: u64,
    pub offering_end: u64,
    pub scheduled_process_time: u64,
    pub started: crate::world::rpc::MetadosisWorldwideDayStartedV1,
    pub status_changes: Vec<crate::world::rpc::MetadosisWorldwideDayStatusChangeV1>,
    pub time_control_epochs: Vec<MetadosisTimeControlEpochV1>,
    pub created_validator_count: u8,
    pub unknown_status_revert_validator_count: u8,
    pub offering_validator_count: u8,
    pub ready_validator_count: u8,
    pub completed_validator_count: u8,
}

/// Runtime proof that proposal, canonical import and late historical replay
/// exercised the same OCOMP boundaries without entering legacy calculation
/// precompiles.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompExecutionTraceObservationV1 {
    pub request_height: u64,
    pub q_forming_height: u64,
    pub proposal_request_nodes: Vec<String>,
    pub canonical_request_nodes: Vec<String>,
    pub canonical_q_vote_nodes: Vec<String>,
    pub historical_replay_node: String,
    pub historical_request_observed: bool,
    pub historical_q_vote_observed: bool,
    pub forbidden_calculation_entries: u64,
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
    pub failed_terminal_receipt: Option<crate::world::rpc::MetadosisWorldwideDayTerminalReceiptV1>,
    pub failed_promis_limit: Option<alloy_primitives::U256>,
    pub failed_terminal_commitment: Option<crate::world::rpc::BlockCommitmentV1>,
    pub held_late_vote_hash: Option<alloy_primitives::B256>,
    pub late_vote_reverted: Option<bool>,
    pub late_vote_inclusion_height: Option<u64>,
    pub capacity_resources: Option<crate::ocomp_capacity::OcompCapacityResourceObservationV1>,
    pub capacity_public_path: Option<OcompPublicCapacityObservationV1>,
    pub capacity_historical_replay: Option<OcompHistoricalReplayObservationV1>,
    pub metadosis_fresh_lifecycle: Option<MetadosisFreshLifecycleObservationV1>,
    pub execution_trace: Option<OcompExecutionTraceObservationV1>,
    pub restart_replay_verified: Option<bool>,
    pub full_node_deadline_barrier_height: Option<u64>,
    pub full_node_resumed_finalized_height: Option<u64>,
    pub full_node_local_first_digest: Option<alloy_primitives::B256>,
    pub full_node_mismatch_job_id: Option<alloy_primitives::B256>,
    pub full_node_mismatch_evidence_files: Vec<String>,
}

/// Per-scenario state accumulated as the steps run.
#[derive(Debug)]
pub struct FixtureState {
    /// Public-only Radicle operations and replication evidence.
    pub radicle: RadicleScenarioEvidenceV1,
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
    /// OS process id captured when a synced FullNode first enters validator mode.
    /// A later activation step uses it to prove DKG did not require a node restart.
    pub promoted_validator_pid: Option<u32>,
    /// The chain's worldwide-day key used for tribute offers.
    pub wwd: Option<String>,
    /// A height captured by one step for a later assertion (kill/restart/exit).
    pub marker_height: Option<u64>,
    /// A log-line count captured before an action (e.g. DKG ceremony count).
    pub marker_count: Option<usize>,
    /// Hash of a transaction that cannot be mined, submitted to observe pool
    /// eviction (`features/txpool_eviction.feature`).
    pub stuck_tx_hash: Option<String>,
    /// Sender and exact nonce bound to [`Self::stuck_tx_hash`]. The eviction
    /// assertion uses all three fields so an unrelated stale transaction cannot
    /// satisfy the observability contract.
    pub stuck_tx_sender: Option<String>,
    pub stuck_tx_nonce: Option<u64>,
    /// Exact offer public key observed from a registered joiner's enclave and
    /// matched against canonical chain state before an enclave restart.
    pub joiner_offer_public_before_restart: Option<[u8; 32]>,
    /// VRF expiry observed while a permanently stalled frozen DKG target is live.
    pub vrf_expiry_height: Option<u64>,
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
    /// Public canonical NOD materialization evidence captured before restart.
    pub ocomp_nod_materialization: Option<crate::world::rpc::NodMaterializationObservationV1>,
    /// Hash of a duplicate logical offer expected to be rejected without state changes.
    pub duplicate_tribute_tx_hash: Option<String>,
    /// Exact primary/owner/day Mongo documents before a duplicate offer.
    pub tribute_projection_before_duplicate:
        Option<crate::world::mongodb::TributeProjectionSnapshot>,
    /// Finalized height immediately before one typed OCOMP process fault.
    pub ocomp_finality_before_fault: Option<u64>,
    /// Finalized height immediately before the managed projection database is
    /// paused in the dirty-operations acceptance lane.
    pub projection_outage_finalized_before: Option<u64>,
    /// Immutable activation height loaded from the scenario's prepared genesis
    /// install. Fresh Measurement activates at block 1.
    pub ocomp_activation_height: Option<u64>,
    /// Public, finalized Metadosis request observed identically on every
    /// validator. This is evidence only; the harness cannot create the job.
    pub ocomp_job_request: Option<crate::world::rpc::OcompPublicJobRequestV1>,
    /// The two independently scheduled WWDs and processing timestamps used by
    /// the dynamic-membership overlap scenario.
    pub ocomp_dynamic_worldwide_days: Vec<u32>,
    pub ocomp_dynamic_processing_times: Vec<u64>,
    /// Public Tribute transaction hashes for those WWDs, in schedule order.
    pub ocomp_dynamic_tribute_tx_hashes: Vec<String>,
    /// Public finalized requests for job A and job B, in that order.
    pub ocomp_dynamic_job_requests: Vec<crate::world::rpc::OcompPublicJobRequestV1>,
    /// Canonical accountability slots populated for job A and job B.
    pub ocomp_dynamic_vote_slots: Vec<Vec<u16>>,
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
    /// Canonical FAILED outcome captured after the bounded OCOMP attempt budget
    /// is exhausted. These values are replay assertions, never control input.
    pub ocomp_failed_terminal_receipt:
        Option<crate::world::rpc::MetadosisWorldwideDayTerminalReceiptV1>,
    pub ocomp_failed_promis_limit: Option<alloy_primitives::U256>,
    pub ocomp_failed_terminal_commitment: Option<crate::world::rpc::BlockCommitmentV1>,
    /// A locally signed OCOMP transaction held by the deadline scenario until the
    /// exclusive boundary. Raw bytes are never published as scenario evidence.
    pub ocomp_held_late_vote_raw: Option<Vec<u8>>,
    pub ocomp_held_late_vote_hash: Option<alloy_primitives::B256>,
    pub ocomp_late_vote_reverted: Option<bool>,
    pub ocomp_late_vote_inclusion_height: Option<u64>,
    pub ocomp_capacity_observation: Option<OcompPublicCapacityObservationV1>,
    pub ocomp_historical_replay_observation: Option<OcompHistoricalReplayObservationV1>,
    pub metadosis_fresh_lifecycle_observation: Option<MetadosisFreshLifecycleObservationV1>,
    pub metadosis_fresh_initial_timestamp: Option<u64>,
    pub metadosis_fresh_initial_unix_time_offset_secs: Option<i64>,
    pub ocomp_execution_trace_observation: Option<OcompExecutionTraceObservationV1>,
    pub ocomp_restart_replay_verified: Option<bool>,
    /// FullNode-only lifecycle evidence captured by the Citadel closure lane.
    pub ocomp_full_node_deadline_barrier_height: Option<u64>,
    pub ocomp_full_node_resumed_finalized_height: Option<u64>,
    pub ocomp_full_node_local_result_before_restart: Option<Vec<u8>>,
    pub ocomp_full_node_local_first_digest: Option<alloy_primitives::B256>,
    pub ocomp_full_node_mismatch_job_id: Option<alloy_primitives::B256>,
    pub ocomp_full_node_mismatch_evidence_files: Vec<String>,
    /// Public pre-activation `submitLysisResult` outcome: inclusion evidence
    /// that the selector reverts (never aborts payload building) while the
    /// OCOMP lifecycle is inactive.
    pub metadosis_inactive_lysis_vote_hash: Option<String>,
    pub metadosis_inactive_lysis_vote_block: Option<u64>,
    pub metadosis_inactive_lysis_reject_code: Option<u64>,

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
    /// Validator-0 Gem balance captured before the stale daily boundary.
    pub reward_gem_balance_before_delivery: Option<alloy_primitives::U256>,
    /// Exact UTC reward day durably stored at the pending Rewards FIFO head.
    pub pending_reward_gem_utc_day: Option<u32>,
    /// Finalized EVM block captured before restarting with a pending Gem batch.
    pub pending_reward_gem_restart_block_number: Option<u64>,
    /// Canonical block that delivered the saved batch through OSG2.
    pub reward_gem_delivery_block_number: Option<u64>,
    /// Exact validator-0 Gem id created by the recovered delivery.
    pub delivered_reward_gem_id: Option<alloy_primitives::U256>,

    // ---- Stablecoin Factory V1 live scenario ----
    pub stablecoin: Option<StablecoinFixture>,

    // ---- Local target chain ----
    /// Addresses the target-chain deploy reported.
    pub target_contracts: Option<crate::world::target_chain::TargetContracts>,
    /// Addresses the origin-side deploy reported.
    pub origin_contracts: Option<crate::world::origin_venue::OriginContracts>,
    /// Units of each lifecycle series settled so far.
    pub settled_units: u32,
    /// The series the lifecycle scenario issued, in the order it issued them.
    pub lifecycle_series: Vec<alloy_primitives::FixedBytes<14>>,
    /// The stablecoin holders settle Intex in, and its reserve vault.
    pub settlement_currency: Option<crate::world::settlement_currency::SettlementCurrency>,
    pub auction_bidders: Vec<crate::world::bidders::Bidder>,
}

#[derive(Debug)]
pub struct StablecoinFixture {
    pub issuer_key: String,
    pub second_issuer_key: String,
    pub recipient_key: String,
    pub spender_key: String,
    pub issuer: alloy_primitives::Address,
    pub second_issuer: alloy_primitives::Address,
    pub recipient: alloy_primitives::Address,
    pub spender: alloy_primitives::Address,
    pub policy_id: alloy_primitives::U256,
    pub proposal_id: u64,
    pub token_id: alloy_primitives::B256,
    pub token: alloy_primitives::Address,
    pub created_block: u64,
    pub snapshot_height: Option<u64>,
    pub snapshot_supply: Option<alloy_primitives::U256>,
    pub snapshot_balances: Option<[alloy_primitives::U256; 3]>,
}

impl Default for FixtureState {
    fn default() -> Self {
        Self {
            radicle: RadicleScenarioEvidenceV1::default(),
            settlement_currency: None,
            lifecycle_series: Vec::new(),
            settled_units: 0,
            proposal_id: 1,
            proposed_version: None,
            activation_height: None,
            vote_deadline: None,
            voting_window: 6,
            allow_unsupported_update_fatal: false,
            expected_dkg_reveal: None,
            joiner_addr: None,
            promoted_validator_pid: None,
            wwd: None,
            marker_height: None,
            marker_count: None,
            stuck_tx_hash: None,
            stuck_tx_sender: None,
            stuck_tx_nonce: None,
            joiner_offer_public_before_restart: None,
            vrf_expiry_height: None,
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
            ocomp_nod_materialization: None,
            duplicate_tribute_tx_hash: None,
            tribute_projection_before_duplicate: None,
            ocomp_finality_before_fault: None,
            projection_outage_finalized_before: None,
            ocomp_activation_height: None,
            ocomp_job_request: None,
            ocomp_dynamic_worldwide_days: Vec::new(),
            ocomp_dynamic_processing_times: Vec::new(),
            ocomp_dynamic_tribute_tx_hashes: Vec::new(),
            ocomp_dynamic_job_requests: Vec::new(),
            ocomp_dynamic_vote_slots: Vec::new(),
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
            ocomp_failed_terminal_receipt: None,
            ocomp_failed_promis_limit: None,
            ocomp_failed_terminal_commitment: None,
            ocomp_held_late_vote_raw: None,
            ocomp_held_late_vote_hash: None,
            ocomp_late_vote_reverted: None,
            ocomp_late_vote_inclusion_height: None,
            metadosis_inactive_lysis_vote_hash: None,
            metadosis_inactive_lysis_vote_block: None,
            metadosis_inactive_lysis_reject_code: None,
            ocomp_capacity_observation: None,
            ocomp_historical_replay_observation: None,
            metadosis_fresh_lifecycle_observation: None,
            metadosis_fresh_initial_timestamp: None,
            metadosis_fresh_initial_unix_time_offset_secs: None,
            ocomp_execution_trace_observation: None,
            ocomp_restart_replay_verified: None,
            ocomp_full_node_deadline_barrier_height: None,
            ocomp_full_node_resumed_finalized_height: None,
            ocomp_full_node_local_result_before_restart: None,
            ocomp_full_node_local_first_digest: None,
            ocomp_full_node_mismatch_job_id: None,
            ocomp_full_node_mismatch_evidence_files: Vec::new(),
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
            reward_gem_balance_before_delivery: None,
            pending_reward_gem_utc_day: None,
            pending_reward_gem_restart_block_number: None,
            reward_gem_delivery_block_number: None,
            delivered_reward_gem_id: None,
            stablecoin: None,
            target_contracts: None,
            origin_contracts: None,
            auction_bidders: Vec::new(),
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
            failed_terminal_receipt: self.ocomp_failed_terminal_receipt.clone(),
            failed_promis_limit: self.ocomp_failed_promis_limit,
            failed_terminal_commitment: self.ocomp_failed_terminal_commitment.clone(),
            held_late_vote_hash: self.ocomp_held_late_vote_hash,
            late_vote_reverted: self.ocomp_late_vote_reverted,
            late_vote_inclusion_height: self.ocomp_late_vote_inclusion_height,
            capacity_resources: None,
            capacity_public_path: self.ocomp_capacity_observation.clone(),
            capacity_historical_replay: self.ocomp_historical_replay_observation.clone(),
            metadosis_fresh_lifecycle: self.metadosis_fresh_lifecycle_observation.clone(),
            execution_trace: self.ocomp_execution_trace_observation.clone(),
            restart_replay_verified: self.ocomp_restart_replay_verified,
            full_node_deadline_barrier_height: self.ocomp_full_node_deadline_barrier_height,
            full_node_resumed_finalized_height: self.ocomp_full_node_resumed_finalized_height,
            full_node_local_first_digest: self.ocomp_full_node_local_first_digest,
            full_node_mismatch_job_id: self.ocomp_full_node_mismatch_job_id,
            full_node_mismatch_evidence_files: self.ocomp_full_node_mismatch_evidence_files.clone(),
        }
    }
}
