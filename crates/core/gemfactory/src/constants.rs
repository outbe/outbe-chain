use alloy_primitives::Address;

pub const NATIVE_TOKEN: &str = "COEN";
pub const STABLECOIN: &str = "0xUSD";

/// Floor-price markup over the COEN market price, expressed as a percent.
/// Agent-flow floor = rate × 108 / 100.
pub const FLOOR_MARKUP_PERCENT: u64 = 108;

/// SRA cost-amount discount, expressed as a percent. Cost = entry × load × 64 / 100.
pub const SRA_COEFFICIENT_PERCENT: u64 = 64;
