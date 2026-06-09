use alloy_primitives::Address;

pub const NATIVE_TOKEN: &str = "COEN";
pub const STABLECOIN: &str = "0xUSD";

/// Floor-price markup over the COEN market price, expressed as a percent.
/// Agent-flow floor = rate × 108 / 100.
pub const FLOOR_MARKUP_PERCENT: u64 = 108;

/// SRA cost-amount discount, expressed as a percent. Cost = entry × load × 64 / 100.
pub const SRA_COEFFICIENT_PERCENT: u64 = 64;

/// Vault provider that custodies the deposited stablecoin on settle. The
/// stablecoin asset itself is resolved at runtime by calling
/// `IVaultProvider.assetAt(0)` on this address.
pub const RESERVE_VAULT: Address =
    alloy_primitives::address!("0xC8ce1EFE882B0fbb1E2ABBEed828316bb282b76d");
