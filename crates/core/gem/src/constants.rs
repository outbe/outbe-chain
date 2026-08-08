pub const TOKEN_NAME: &str = "Gem";
pub const TOKEN_SYMBOL: &str = "GEM";
pub const TOKEN_DESCRIPTION: &str = "Outbe Gem";
pub const TOKEN_IMAGE_BASE: &str = "https://api.outbe.io/gem/image/";

/// ISO 4217 code the qualifier hook consults each block. The oracle pair is
/// derived from the code as `COEN/<iso>` at runtime. Only gems whose
/// `reference_currency` equals this code participate in qualification —
/// others are silently skipped so they don't get promoted against an
/// unrelated rate.
pub const QUALIFIER_REFERENCE_ISO: u16 = 840;

pub const BIN_STEP_BP: u16 = 25;

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
