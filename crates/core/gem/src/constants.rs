pub const TOKEN_NAME: &str = "Gem";
pub const TOKEN_SYMBOL: &str = "GEM";
pub const TOKEN_DESCRIPTION: &str = "Outbe Gem";
pub const TOKEN_IMAGE_BASE: &str = "https://api.outbe.io/gem/image/";

/// ISO 4217 code the qualifier hook consults each block. The actual oracle
/// pair is resolved via `settlement_iso_to_pair` at runtime. Only gems whose
/// `reference_currency` equals this code participate in qualification —
/// others are silently skipped so they don't get promoted against an
/// unrelated rate.
pub const QUALIFIER_REFERENCE_ISO: u16 = 840;

pub const BIN_STEP_BP: u16 = 25;

/// Call-trigger evaluation window: most recent completed days scanned for
/// breaches of a gem's Call Threshold.
pub const GEM_CALL_WINDOW_DAYS: u16 = 30;

/// Breach-days within the window required to force-call a gem
/// (coen VWAP above the Call Threshold on this many of the last window days).
pub const GEM_CALL_THRESHOLD_DAYS: u16 = 20;

/// Call Notice Period: seconds after `called_at` within which the holder must
/// settle. Once elapsed the gem is forfeit-burned. 8 days.
pub const GEM_CALL_PERIOD_SECONDS: u32 = 8 * 24 * 3600;
