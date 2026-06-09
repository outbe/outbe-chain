//! Constants for the zero-fee paymaster sponsorship policy.
//!
//! These are part of the protocol contract — changing them is a hard-fork
//! event. Fee shape errors share the generic `FeeCapTooLow` (code 105)
//! with the oracle hook because both paths require `priority_fee == 0`
//! and `max_fee >= MIN_PROTOCOL_BASE_FEE`. All other free-tx-specific
//! reasons occupy dedicated codes 110..=116:
//!
//! - 110 `FreeTxDailyExhausted` — daily quota burned
//! - 111 `FreeTxDailyNoExistingAccount` — anti-sybil (`balance == 0`)
//! - 112 `FreeTxDailyContractCreationForbidden` — `to == None`
//! - 113 `FreeTxDailyValueNotZero` — `msg.value != 0`
//! - 114 `FreeTxDailyGasLimitExceeded` — `gas_limit > FREE_TX_DAILY_GAS_LIMIT`
//! - 115 `FreeTxDailyCalldataTooLarge` — `calldata > FREE_TX_DAILY_CALLDATA_BYTES`
//! - 116 `FreeTxDailyTargetNotWhitelisted` — `to ∉ SPONSORED_TARGET_WHITELIST`
//!
//! See `hooks.rs::ZeroFeePolicyError::code` for the authoritative mapping.

use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;

/// Maximum number of sponsored free transactions per signer per UTC day.
pub const FREE_TX_DAILY_LIMIT: u32 = 8;

/// Maximum `gas_limit` accepted for a sponsored free transaction.
///
/// Caps the per-tx compute budget so 8 × N sybil-funded addresses cannot
/// exhaust a block on the sponsored path. 200_000 covers ERC-20 transfer
/// plus a small log, matching the typical onboarding interaction.
pub const FREE_TX_DAILY_GAS_LIMIT: u64 = 200_000;

/// Maximum calldata size accepted for a sponsored free transaction.
///
/// Mirrors the existing oracle zero-fee envelope cap to prevent calldata
/// DoS through the free path.
pub const FREE_TX_DAILY_CALLDATA_BYTES: usize = 16 * 1024;

/// Minimum EIP-1559 fee cap accepted by Reth's public txpool.
///
/// Mirrors the oracle hook's threshold so both zero-fee paths agree on
/// the same lower bound. A free tx with `max_fee_per_gas` below this is
/// rejected with [`super::hooks::ZeroFeePolicyError::FeeCapTooLow`].
pub const MIN_FREE_TX_MAX_FEE_PER_GAS: u128 = MIN_PROTOCOL_BASE_FEE as u128;
