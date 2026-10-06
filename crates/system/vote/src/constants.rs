/// Compile-time default for the governance voting window, in blocks.
///
/// Runtime reads `get_governance_voting_window_blocks`.
/// That getter can override this default.
/// Tests are the only users of this constant.
pub const VOTING_WINDOW_BLOCKS: u64 =
    outbe_chain_constants::DEFAULT_GOVERNANCE_VOTING_WINDOW_BLOCKS;

/// Quorum numerator for `yes_votes / active_validator_count`.
pub const QUORUM_NUMERATOR: u64 = 2;

/// Quorum denominator for `yes_votes / active_validator_count`.
pub const QUORUM_DENOMINATOR: u64 = 3;

/// Maximum number of proposals in the bounded pending index.
pub const MAX_PENDING_PROPOSALS: u32 = 64;

/// Maximum pending proposals a single active validator may have open at once.
pub const MAX_PENDING_PROPOSALS_PER_VALIDATOR: u32 = 1;

/// Maximum page size for paginated vote reads.
pub const MAX_PAGE_SIZE: u64 = 100;
