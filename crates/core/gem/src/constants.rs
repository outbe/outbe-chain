pub const TOKEN_NAME: &str = "Gem";
pub const TOKEN_SYMBOL: &str = "GEM";
pub const TOKEN_DESCRIPTION: &str = "Outbe Gem";
pub const TOKEN_IMAGE_BASE: &str = "https://api.outbe.io/gem/image/";

pub const BIN_STEP_BP: u16 = 25;

/// Gems a begin-block qualify scan may inspect, shared across all reference
/// currencies. The per-currency bin cursor resumes the rest next block.
pub const MAX_GEM_QUALIFICATIONS_PER_BLOCK: u32 = 256;

/// Gems one call slice may call before it gives out; the sweep resumes on the
/// next block.
pub const MAX_GEM_CALLS_PER_BLOCK: u32 = 256;

/// Gems one block's expiry sweep may forfeit. Each burn compacts the owner's gem
/// list, which is linear in what that owner holds, and a begin-block hook is not
/// gas-metered - so this is deliberately far below the other per-block budgets.
/// It still clears far more per day than the daily sweep it replaced.
pub const MAX_GEM_FORFEITS_PER_BLOCK: u32 = 16;

/// Call-trigger evaluation window in seconds (28 days): span scanned for
/// breaches of a gem's Call Threshold. The daily scan divides by 86400.
pub const CALL_WINDOW: u32 = 28 * 24 * 3600;

/// Ceiling on the days one call scan reads per currency. The collected span widens
/// with the windows ever issued, so a corrupt record must not turn into an unbounded
/// oracle read; the Oracle backfills no further than a year anyway.
pub(crate) const MAX_CALL_WINDOW_DAYS: u32 = 366;

/// Breach threshold in seconds (21 days): a gem force-calls when the coen VWAP
/// breaches its Call Price on 21 of the window's 28 days. The daily scan
/// divides by 86400 to get the day count.
pub const CALL_THRESHOLD: u32 = 21 * 24 * 3600;

/// Call Notice Period in seconds (7 days): time after `called_at` within which
/// the holder must settle. Once elapsed the gem is forfeit-burned.
pub const CALL_NOTICE_PERIOD: u32 = 7 * 24 * 3600;

/// Bucket slots one expiry sweep may look at per block. An empty or not-yet-due
/// slot still costs a read, so without its own budget one long bucket would be walked
/// end to end in a single block.
pub(crate) const MAX_EXPIRY_SLOTS_PER_BLOCK: u32 = 256;

/// Deadline buckets one expiry sweep may open per block. Each costs a tree
/// descent plus its own bookkeeping, so a long backlog of buckets spreads over blocks
/// the same way a long bucket does.
pub(crate) const MAX_EXPIRY_BUCKETS_PER_BLOCK: u32 = 8;
