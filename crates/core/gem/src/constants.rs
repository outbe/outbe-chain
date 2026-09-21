pub const TOKEN_NAME: &str = "Gem";
pub const TOKEN_SYMBOL: &str = "GEM";
pub const TOKEN_DESCRIPTION: &str = "Outbe Gem";
pub const TOKEN_IMAGE_BASE: &str = "https://api.outbe.io/gem/image/";

pub const BIN_STEP_BP: u16 = 25;

/// Gems one call slice may call before it gives out; the sweep resumes on the
/// next block.
pub const MAX_GEM_CALLS_PER_BLOCK: u32 = 256;

/// `SweepDaySkipped.sweep` of the Called sweep; 0 belonged to the retired qualify sweep.
pub const CALL_SWEEP: u8 = 1;

/// Slots one block's expiry sweep may step through. Low because a forfeit compacts
/// the owner's whole gem list and a block hook is not gas-metered.
pub const MAX_EXPIRY_STEPS_PER_BLOCK: u32 = 16;

/// Call-trigger evaluation window in seconds (28 days): span scanned for
/// breaches of a gem's Call Threshold. The daily scan divides by 86400.
pub const CALL_WINDOW: u32 = 28 * 24 * 3600;

/// Breach threshold in seconds (21 days): a gem force-calls when the coen VWAP
/// breaches its Call Price on 21 of the window's 28 days. The daily scan
/// divides by 86400 to get the day count.
pub const CALL_THRESHOLD: u32 = 21 * 24 * 3600;

/// Call Notice Period in seconds (7 days): time after `called_at` within which
/// the owner must settle. Once elapsed the gem is forfeit-burned.
pub const CALL_NOTICE_PERIOD: u32 = 7 * 24 * 3600;

/// GemPosition validity period: an Intex in the Gem Factory expires this long after
/// `issued_at`; no new gems may be issued afterward. 1 year.
pub const POSITION_VALIDITY_SECONDS: u64 = 365 * 24 * 3600;

/// Floor-price markup rate: floor = `entry x (100 + FLOOR_RATE) / 100`.
pub const FLOOR_RATE: u16 = 8;

/// Call-price markup rate: call price = `entry x (100 + CALL_RATE) / 100`.
/// Its breach arms a Call Event.
pub const CALL_RATE: u16 = 128;
