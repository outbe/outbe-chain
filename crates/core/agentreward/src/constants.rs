//! AgentReward protocol parameters.

/// Most WAA and SRA addresses that one block credits.
pub const ACCRUAL_RECIPIENTS_PER_BLOCK: u32 = 256;

/// Percentage points off the previous-day VWAP that every agent reward Gem enters at.
pub const AGENT_DISCOUNT_PCT: u16 = 4;
