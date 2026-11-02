//! AgentReward protocol parameters.

/// Idle lifetime of one account's nonempty reward pool, in timestamp seconds.
pub const REWARD_BALANCE_IDLE_SECONDS: u64 = 365 * outbe_primitives::time::SECONDS_PER_DAY;

/// Shared recipient-entry visit budget across all four pools and queued days.
/// Each cleanup owner has a separate budget of this size; WAA/SRA share one owner.
pub const ACCRUAL_RECIPIENTS_PER_BLOCK: u32 = 256;

/// Percentage points off the previous-day VWAP that every agent reward Gem enters at.
pub const AGENT_DISCOUNT_PCT: u16 = 4;
