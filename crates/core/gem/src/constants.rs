pub const TOKEN_NAME: &str = "Gem";
pub const TOKEN_SYMBOL: &str = "GEM";
pub const TOKEN_DESCRIPTION: &str = "Outbe Gem";
pub const TOKEN_IMAGE_BASE: &str = "https://api.outbe.io/gem/image/";

pub const MATURITY_PERIOD_SECONDS: u64 = 21 * 24 * 60 * 60;

/// ISO 4217 code the qualifier hook consults each block. The actual oracle
/// pair is resolved via `settlement_iso_to_pair` at runtime. Only gems whose
/// `reference_currency` equals this code participate in qualification —
/// others are silently skipped so they don't get promoted against an
/// unrelated rate.
pub const QUALIFIER_REFERENCE_ISO: u16 = 840;

pub const BIN_STEP_BP: u16 = 25;
