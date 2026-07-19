//! Mutable fixture state threaded across a scenario's steps.
//!
//! Values a step computes and a later step reads back (the proposal under test,
//! the version/heights we proposed, the deadline we observed). Kept off the
//! handles so `localnet`/`rpc`/`validators` stay stateless verbs.

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

    // ---- validator-lifecycle scenarios (s1..s7 / follower) ----
    /// Provisioned joiner's EOA address (derived after `provision`).
    pub joiner_addr: Option<String>,
    /// The chain's worldwide-day key used for tribute offers.
    pub wwd: Option<String>,
    /// A height captured by one step for a later assertion (kill/restart/exit).
    pub marker_height: Option<u64>,
    /// A log-line count captured before an action (e.g. DKG ceremony count).
    pub marker_count: Option<usize>,
    /// Worldwide-day status byte captured before a tribute offer (invariant check).
    pub wwd_status_before: Option<String>,
    /// Exact lifecycle accounting snapshot captured immediately before exit.
    pub lifecycle_stake_before_exit: Option<alloy_primitives::U256>,
    pub lifecycle_total_before_exit: Option<alloy_primitives::U256>,
    pub lifecycle_staking_balance_before_exit: Option<alloy_primitives::U256>,
    /// Hash of the encrypted tribute transaction under projection verification.
    pub tribute_tx_hash: Option<String>,
    /// Hash of a duplicate logical offer expected to be rejected without state changes.
    pub duplicate_tribute_tx_hash: Option<String>,

    // ---- ZeroFee live scenario ----
    pub zerofee_key: Option<String>,
    pub zerofee_address: Option<String>,
    pub zerofee_delegation_receipt: Option<serde_json::Value>,
    pub zerofee_sponsored_receipts: Vec<serde_json::Value>,
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
}

impl Default for FixtureState {
    fn default() -> Self {
        Self {
            proposal_id: 1,
            proposed_version: None,
            activation_height: None,
            vote_deadline: None,
            voting_window: 6,
            joiner_addr: None,
            wwd: None,
            marker_height: None,
            marker_count: None,
            wwd_status_before: None,
            lifecycle_stake_before_exit: None,
            lifecycle_total_before_exit: None,
            lifecycle_staking_balance_before_exit: None,
            tribute_tx_hash: None,
            duplicate_tribute_tx_hash: None,
            zerofee_key: None,
            zerofee_address: None,
            zerofee_delegation_receipt: None,
            zerofee_sponsored_receipts: Vec::new(),
            zerofee_balance_before: None,
            zerofee_balance_after_quota: None,
            zerofee_ninth_receipt: None,
            zerofee_balance_after_ninth: None,
            zerofee_paid_receipt: None,
            zerofee_balance_after_paid: None,
            zerofee_delegation_raw: None,
            zerofee_replay_error: None,
        }
    }
}
