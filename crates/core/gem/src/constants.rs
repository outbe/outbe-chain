pub const TOKEN_NAME: &str = "Gem";
pub const TOKEN_SYMBOL: &str = "GEM";
pub const TOKEN_DESCRIPTION: &str = "Outbe Gem";
pub const TOKEN_IMAGE_BASE: &str = "https://api.outbe.io/gem/image/";

pub const BIN_STEP_BP: u16 = 25;

/// Gems a begin-block qualify scan may inspect, shared across all reference
/// currencies. The per-currency bin cursor resumes the rest next block.
pub const MAX_GEM_QUALIFICATIONS_PER_BLOCK: u32 = 256;

/// Gems the daily run may call, and separately forfeit, before it gives out.
/// A correlated breach calls a whole currency at once and their notice periods
/// then lapse together, so both arms need a ceiling.
pub const MAX_GEM_CALL_ACTIONS_PER_RUN: u32 = 256;

/// Call-trigger evaluation window in seconds (28 days): span scanned for
/// breaches of a gem's Call Threshold. The daily scan divides by 86400.
pub const CALL_WINDOW: u32 = 28 * 24 * 3600;

/// Breach threshold in seconds (21 days): a gem force-calls when the coen VWAP
/// breaches its Call Price on 21 of the window's 28 days. The daily scan
/// divides by 86400 to get the day count.
pub const CALL_THRESHOLD: u32 = 21 * 24 * 3600;

/// Call Notice Period in seconds (7 days): time after `called_at` within which
/// the holder must settle. Once elapsed the gem is forfeit-burned.
pub const CALL_NOTICE_PERIOD: u32 = 7 * 24 * 3600;
