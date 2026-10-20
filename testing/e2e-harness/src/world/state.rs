//! Mutable fixture state threaded across a scenario's steps.
//!
//! It holds values that a step computes and a later step reads back (the proposal
//! under test, the version/heights we proposed, the deadline we observed). This
//! state stays off the handles so `localnet`/`rpc`/`validators` stay stateless verbs.

use serde::Serialize;

/// Original owned committee incarnations. This record contains no signing material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DowntimeNode {
    pub index: usize,
    pub port: u16,
    pub node_pid: u32,
    pub enclave_pid: u32,
}

/// Punitive accounting surfaces read at one exact finalized block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DowntimeAccounting {
    pub stake: alloy_primitives::U256,
    pub mirrored_stake: alloy_primitives::U256,
    pub total_staked: alloy_primitives::U256,
    pub staking_balance: alloy_primitives::U256,
    pub status: u8,
    pub slash_count: u64,
    pub felony_count: u64,
    pub unbonding_head: alloy_primitives::U256,
    pub slash_config: [alloy_primitives::U256; 2],
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DowntimeObservation {
    pub height: u64,
    pub block_hash: alloy_primitives::B256,
    pub state_root: alloy_primitives::B256,
    pub accounts: Vec<(u16, DowntimeAccounting)>,
}

/// Decoded canonical public event identity, retained across the no-repeat wait.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DowntimeFelonyEvent {
    pub validator: alloy_primitives::Address,
    pub miss_count: u64,
    pub felony_count: u64,
    pub height: u64,
    pub block_hash: alloy_primitives::B256,
    pub transaction_hash: alloy_primitives::B256,
    pub log_index: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct DowntimeState {
    pub victim: alloy_primitives::Address,
    pub victim_index: usize,
    pub nodes: Vec<DowntimeNode>,
    pub percent: u64,
    pub threshold: u64,
    pub before: DowntimeObservation,
    pub fault_target: Option<u64>,
    pub penalty: Option<DowntimeObservation>,
    pub event: Option<DowntimeFelonyEvent>,
}

/// Owned replacement processes and their launch-scoped observations.
#[derive(Debug)]
pub(crate) struct RestartIncarnation {
    pub node_pid: u32,
    pub enclave_pid: u32,
    pub node_log: crate::internal::launch_log::LaunchLog,
    pub enclave_log: crate::internal::launch_log::LaunchLog,
}

/// Cleanup evidence retained only after the complete DKG expiry assertion has
/// revalidated the sealed halt witness and its original owned processes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DkgExpiryExpectedExit {
    pub slot: usize,
    pub node_pid: u32,
}

/// Public DKG identity retained across the completed-but-pending crash point.
#[derive(Debug)]
pub(crate) struct PendingDkgRestartState {
    pub checkpoint: crate::internal::pending_dkg::PendingDkgCheckpoint,
    pub keys_dir: std::path::PathBuf,
    pub consensus_public_key: Vec<u8>,
    pub before: crate::world::rpc::FinalizedCheckpoint,
    pub original_pids: (u32, u32),
    pub replacement: Option<RestartIncarnation>,
}

/// Immutable public observations captured by steps, before after-hook teardown.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TeeLeaseEvidenceV1 {
    pub observations: Vec<serde_json::Value>,
    pub full_node_shutdown: Option<TeeLeaseShutdownV1>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TeeLeaseLogIntervalV1 {
    pub path: std::path::PathBuf,
    pub device: u64,
    pub inode: u64,
    pub start: u64,
    pub end: u64,
    pub content: String,
}

/// Constructed only after observing the original owned child exit, before replacement.
#[derive(Clone, Debug, Serialize)]
pub struct TeeLeaseShutdownV1 {
    pub slot: usize,
    pub pid: u32,
    pub deadline: u64,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub sinks: Vec<TeeLeaseLogIntervalV1>,
}

/// Pending-pool fixture identity. Secret bytes stay in a private scenario file.
#[derive(Clone, Debug)]
pub(crate) struct PendingValidatorPoolFixture {
    pub key_file: std::path::PathBuf,
    pub sender: alloy_primitives::Address,
    pub hash: String,
    pub nonce: u64,
    pub gas_limit: u64,
    pub max_fee: u128,
    pub admitted_tip_timestamp: u64,
    pub owner_pid: u32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RadicleScenarioEvidenceV1 {
    pub governments_bootstrap: Vec<serde_json::Value>,
    pub governments_patch: Option<GovernmentsPatchEvidenceV1>,
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

/// Real-chain identifiers and observations for the governments patch Vote scenario.
#[derive(Clone, Debug, Default, Serialize)]
pub struct GovernmentsPatchEvidenceV1 {
    pub author: String,
    pub patch_id: String,
    pub revision_id: String,
    pub base: String,
    pub head: String,
    pub proposal_id: Option<u64>,
    pub proposal_transaction: Option<String>,
    pub vote_transactions: Vec<String>,
    pub approval: Option<serde_json::Value>,
    pub validators: Vec<serde_json::Value>,
    pub fresh_validator: Option<serde_json::Value>,
    pub ordinary_reader: Option<serde_json::Value>,
}

/// Public AgentReward observations attached to the native Lysis production-path
/// scenario. Beneficiary private keys remain deterministic harness constants
/// and are deliberately excluded from scenario evidence.
#[derive(Clone, Debug, Serialize)]
pub struct AgentRewardObservationV1 {
    pub waa_beneficiary: alloy_primitives::Address,
    pub sra_beneficiary: alloy_primitives::Address,
    pub offer_execution_block_number: Option<u64>,
    pub offer_execution_timestamp: Option<u64>,
    pub reward_utc_day: Option<u32>,
    pub escrow_before_settlement_coen_units: Option<alloy_primitives::U256>,
    pub cca_before_settlement_coen_units: Option<alloy_primitives::U256>,
    pub waa_claimable_minor: Option<alloy_primitives::U256>,
    pub sra_claimable_minor: Option<alloy_primitives::U256>,
    pub claim_finalized_height: Option<u64>,
    pub before_settlement_checkpoint: Option<AgentRewardCheckpointV1>,
    pub economic_check: Option<AgentRewardEconomicCheckV1>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct AgentRewardCheckpointV1 {
    pub height: u64,
    pub block_hash: alloy_primitives::B256,
    pub state_root: alloy_primitives::B256,
    pub timestamp: u64,
}

/// Expected economics come from the seeded calendar and independent Decimal
/// emission vectors, before comparison with finalized balances on every port.
#[derive(Clone, Debug, Serialize)]
pub struct AgentRewardEconomicCheckV1 {
    pub genesis_rewards_timestamp: u64,
    pub emission_day: u64,
    pub expected_waa_minor: alloy_primitives::U256,
    pub expected_sra_minor: alloy_primitives::U256,
    pub expected_cca_delta_coen_units: alloy_primitives::U256,
    pub checkpoint: AgentRewardCheckpointV1,
    pub validator_ports: Vec<u16>,
}

/// Per-scenario state accumulated as the steps run.
#[derive(Debug)]
pub struct FixtureState {
    pub(crate) credis: Option<crate::world::credis::CredisFixture>,

    pub(crate) hyperlane_controller:
        Option<crate::features::hyperlane_controller::HyperlaneFixture>,
    pub(crate) tee_observability: Option<crate::features::tee_observability::TeeObservation>,
    pub tee_lease: TeeLeaseEvidenceV1,
    /// Public restart measurements saved before process teardown.
    pub restart_observations: Vec<serde_json::Value>,
    /// Common finalized state captured before the current lifecycle transition.
    pub(crate) lifecycle_before: Option<crate::world::rpc::FinalizedCheckpoint>,
    /// Explicitly expected owned nodes and their current observation intervals.
    pub(crate) lifecycle_incarnations: std::collections::BTreeMap<usize, RestartIncarnation>,
    pub(crate) pending_dkg_restart: Option<PendingDkgRestartState>,
    pub(crate) committee_restart: Vec<RestartIncarnation>,
    pub(crate) downtime: Option<DowntimeState>,
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
    /// fatal compatibility message. Every other fatal/alarm remains forbidden.
    pub allow_unsupported_update_fatal: bool,
    /// One stalled-reshare scenario deliberately leaves this exact participant
    /// offline long enough for the protocol's documented share-reveal path.
    /// Every other reveal/fatal/alarm remains forbidden by the log audit.
    pub expected_dkg_reveal: Option<String>,
    pub(crate) expected_dkg_expiry_exits: Vec<DkgExpiryExpectedExit>,
    /// One manual-lease scenario deliberately fail-stops this validator after
    /// its finalized lease expires. The log audit accepts only the exact two-sink
    /// Reth shutdown trailers that are causally bound to that guard.
    pub expected_tee_lease_guard_shutdown_validator: Option<usize>,
    pub expected_tee_sidecar_outage: Option<(usize, String)>,
    /// The same manual-lease scenario deliberately fail-stops this role-neutral
    /// FullNode. Acceptance requires the owned exit and exact bounded sink records.
    pub expected_tee_lease_guard_shutdown_full_node: Option<TeeLeaseShutdownV1>,

    // ---- validator-lifecycle scenarios (s1..s7 / follower) ----
    /// Provisioned joiner's EOA address (derived after `provision`).
    pub joiner_addr: Option<String>,
    /// OS process id captured when a synced FullNode first enters validator mode.
    /// A later activation step uses it to prove DKG did not require a node restart.
    pub promoted_validator_pid: Option<u32>,
    /// The chain's worldwide-day key used for tribute offers.
    pub wwd: Option<String>,
    pub lysis_completion_height: Option<u64>,
    /// A height captured by one step for a later assertion (kill/restart/exit).
    pub marker_height: Option<u64>,
    pub(crate) chained_handoff: Option<crate::internal::certified_handoff::PinnedHandoff>,
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
    pub(crate) pending_validator_pool: Option<PendingValidatorPoolFixture>,
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
    pub tribute_owner_keys: Vec<String>,
    pub tribute_transactions: Vec<String>,
    /// WAA/SRA public-path observations for the native Tribute.
    pub agent_reward: Option<AgentRewardObservationV1>,
    /// Hash of a duplicate logical offer expected to be rejected without state changes.
    pub duplicate_tribute_tx_hash: Option<String>,
    /// Exact primary/owner/day logical records before a duplicate offer.
    pub tribute_projection_before_duplicate:
        Option<crate::world::projection::TributeProjectionSnapshot>,

    // ---- L2Registry zk-gate scenarios (PFS-001-10 / -11) ----
    /// Hash of an offer expected to be rejected by the zk signature gate.
    pub l2_rejected_offer_tx_hash: Option<String>,
    /// First use the declared L2 57005 binding; allocate additional ids only
    /// when a scenario registers more operators.
    pub l2_next_governed_chain_id: u64,

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
    /// Validator 0's claimed reward Gem, shared by the settlement steps.
    pub(crate) validator_reward_gem: Option<crate::features::settlement::ValidatorRewardGem>,

    // ---- Stablecoin Factory V1 live scenario ----
    pub stablecoin: Option<StablecoinFixture>,

    // ---- Local target chain ----
    /// Addresses the target-chain deploy reported.
    pub target_contracts: Option<crate::world::target_chain::TargetContracts>,
    /// Addresses the origin-side deploy reported.
    pub origin_contracts: Option<crate::world::origin_venue::OriginContracts>,
    /// The Intex series the gem scenario parks to open its position.
    pub gem_source_series: Option<alloy_primitives::FixedBytes<14>>,
    /// The merchant position those parked units opened.
    pub gem_position: Option<alloy_primitives::U256>,
    /// The gem lifecycle's gems, one per owner, in owner order.
    pub lifecycle_gems: Vec<alloy_primitives::U256>,
    /// The Nod lifecycle's Nods, one per owner, in owner order.
    pub lifecycle_nods: Vec<alloy_primitives::U256>,
    /// What the shared lifecycle phases recorded.
    pub entity_lifecycle: crate::features::entity_lifecycle::LifecycleLedger,
    /// The series the lifecycle scenario issued, in the order it issued them.
    pub lifecycle_series: Vec<alloy_primitives::FixedBytes<14>>,
    /// Issued alongside them and only partly settled, so the call notice runs out on
    /// the rest of it.
    pub expiring_series: Option<alloy_primitives::FixedBytes<14>>,
    /// Issued alongside them and never touched at all, so its whole tirage is forfeited.
    pub untouched_series: Option<alloy_primitives::FixedBytes<14>>,
    /// The worldwide day the lifecycle series were issued into; the called group's key.
    pub lifecycle_day: Option<u32>,
    /// Every settlement currency a lifecycle scenario registered, by ISO code.
    pub currencies:
        std::collections::BTreeMap<u16, crate::world::settlement_currency::SettlementCurrency>,
    /// The issuance currency a lifecycle scenario prices besides USD, chosen by its tag.
    pub issuance_market: Option<u16>,
    pub auction_bidders: Vec<crate::world::bidders::Bidder>,

    // ---- Consensus resilience scenarios (B8, R7, R11, R13, S14) ----
    pub resilience_observed_height: Option<u64>,
    pub resilience_ordered_tx_hashes: Vec<String>,
    pub resilience_healthy_tx_hash: Option<String>,
    pub resilience_target_slot: Option<usize>,
    pub resilience_disconnected_height: Option<u64>,
    pub resilience_follower_pid: Option<u32>,
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
            credis: None,

            hyperlane_controller: None,
            radicle: RadicleScenarioEvidenceV1::default(),
            tee_lease: TeeLeaseEvidenceV1::default(),
            tee_observability: None,
            restart_observations: Vec::new(),
            lifecycle_before: None,
            lifecycle_incarnations: std::collections::BTreeMap::new(),
            pending_dkg_restart: None,
            committee_restart: Vec::new(),
            downtime: None,

            currencies: std::collections::BTreeMap::new(),
            issuance_market: None,
            gem_source_series: None,
            gem_position: None,
            lifecycle_gems: Vec::new(),
            lifecycle_nods: Vec::new(),

            entity_lifecycle: Default::default(),
            lifecycle_series: Vec::new(),
            expiring_series: None,
            untouched_series: None,
            lifecycle_day: None,
            proposal_id: 1,
            proposed_version: None,
            activation_height: None,
            vote_deadline: None,
            voting_window: 6,
            allow_unsupported_update_fatal: false,
            expected_dkg_reveal: None,
            expected_dkg_expiry_exits: Vec::new(),
            expected_tee_lease_guard_shutdown_validator: None,
            expected_tee_sidecar_outage: None,
            expected_tee_lease_guard_shutdown_full_node: None,
            joiner_addr: None,
            promoted_validator_pid: None,
            wwd: None,
            lysis_completion_height: None,
            marker_height: None,
            chained_handoff: None,
            marker_count: None,
            stuck_tx_hash: None,
            stuck_tx_sender: None,
            stuck_tx_nonce: None,
            pending_validator_pool: None,
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
            tribute_owner_keys: Vec::new(),
            tribute_transactions: Vec::new(),
            agent_reward: None,
            duplicate_tribute_tx_hash: None,
            tribute_projection_before_duplicate: None,
            l2_rejected_offer_tx_hash: None,
            l2_next_governed_chain_id: 57_005,
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
            validator_reward_gem: None,
            stablecoin: None,
            target_contracts: None,
            origin_contracts: None,
            auction_bidders: Vec::new(),
            resilience_observed_height: None,
            resilience_ordered_tx_hashes: Vec::new(),
            resilience_healthy_tx_hash: None,
            resilience_target_slot: None,
            resilience_disconnected_height: None,
            resilience_follower_pid: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SnapshotBlock {
    pub number: u64,
    /// Native manifest/report spelling: lowercase 64 hex digits, no 0x.
    pub hash: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct SnapshotCommandObservation {
    pub argv: Vec<String>,
    pub started: u64,
    pub ended: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
