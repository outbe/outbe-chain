//! AgentReward protocol parameters.

/// The life of the reward of one pool and day, from the time its rate is set, in seconds.
pub const REWARD_BALANCE_IDLE_SECONDS: u64 = 365 * outbe_primitives::time::SECONDS_PER_DAY;

/// The most days one claim or expiry names.
pub const MAX_REWARD_DAYS_PER_CALL: usize = 64;

/// The fixed point of a reward rate: 18 decimal places.
pub const RATE_SCALE: u64 = 1_000_000_000_000_000_000;

/// Percentage points off the 24-hour VWAP of the claim hour that every agent reward Gem enters at.
pub const AGENT_DISCOUNT_PCT: u16 = 4;
