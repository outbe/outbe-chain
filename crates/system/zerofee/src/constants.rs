//! Constants for the zero-fee paymaster sponsorship policy.
//!
//! These are part of the protocol contract. A change to them is a hard-fork
//! event. Fee shape errors share the generic `FeeCapTooLow` (code 105)
//! with the oracle hook. They share it because both paths require
//! `priority_fee == 0` and `max_fee >= MIN_PROTOCOL_BASE_FEE`. All other free-tx-specific
//! reasons occupy dedicated codes 110..=116:
//!
//! - 110 `FreeTxDailyExhausted` - daily quota burned
//! - 111 retired - formerly rejected zero-balance signers; MUST NOT be reused
//! - 112 `FreeTxDailyContractCreationForbidden` - `to == None`
//! - 113 `FreeTxDailyValueNotZero` - `msg.value != 0`
//! - 114 `FreeTxDailyGasLimitExceeded` - gas exceeds the `(target, selector)` ceiling
//! - 115 `FreeTxDailyCalldataTooLarge` - `calldata > FREE_TX_DAILY_CALLDATA_BYTES`
//! - 116 `FreeTxDailyTargetNotWhitelisted` - `to not in SPONSORED_TARGET_WHITELIST`
//!
//! See `hooks.rs::ZeroFeePolicyError::code` for the authoritative mapping.

use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use outbe_primitives::addresses::TRIBUTE_FACTORY_ADDRESS;
use outbe_primitives::system_tx::STEADY_BLOCK_GAS_LIMIT;

mod tribute_factory_abi {
    alloy_sol_types::sol!("../../../contracts/precompiles/src/ITributeFactory.sol");
}

/// Maximum number of sponsored free transactions per signer per UTC day.
pub const FREE_TX_DAILY_LIMIT: u32 = 8;

/// Default maximum `gas_limit` accepted for a sponsored free transaction.
///
/// Caps per-transaction compute, while [`SPONSORED_GAS_PER_BLOCK`] bounds
/// aggregate sponsored execution in a block. 500_000 covers ERC-20 transfer
/// plus a small log, matching the typical onboarding interaction. The
/// `(target, selector)` grants in [`ELEVATED_SPONSORED_GAS`] can select a
/// larger ceiling for specific operations.
pub const FREE_TX_DAILY_GAS_LIMIT: u64 = 500_000;

/// Maximum gas limit for the one-time self-authorized EIP-7702 bootstrap.
///
/// The bootstrap transaction only installs the canonical ZeroFee delegation
/// and calls the paymaster's read-only authorization view. It is deliberately
/// narrower than the ordinary sponsored-call budget.
pub const FREE_TX_BOOTSTRAP_GAS_LIMIT: u64 = 100_000;

/// Maximum sponsored gas limit for `TributeFactory.offerTribute` calls.
///
/// This matches the explicit transaction limit that `outbe-cli tribute
/// offer` uses. It leaves headroom above the verifier's 300,000 base gas. It
/// does not broaden the limit for other selectors or sponsored targets.
pub const FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT: u64 = 8_000_000;

/// Operation-specific sponsored gas ceilings, keyed by `(target, selector)`.
///
/// `offerTribute` performs UltraHonk proof verification with a 300,000 base
/// gas charge. This grant preserves its existing 8M headroom without
/// reassessing that ceiling. Selector matching does not validate arguments.
pub const ELEVATED_SPONSORED_GAS: &[(Address, [u8; 4], u64)] = &[(
    TRIBUTE_FACTORY_ADDRESS,
    tribute_factory_abi::ITributeFactory::offerTributeCall::SELECTOR,
    FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT,
)];

// Reject duplicate keys even when their gas ceilings differ.
const _: () = {
    let list = ELEVATED_SPONSORED_GAS;
    let mut i = 0;
    while i < list.len() {
        let mut j = i + 1;
        while j < list.len() {
            let a = list[i].0 .0 .0;
            let b = list[j].0 .0 .0;
            let mut k = 0;
            let mut eq = true;
            while k < 20 {
                if a[k] != b[k] {
                    eq = false;
                    break;
                }
                k += 1;
            }
            k = 0;
            while eq && k < 4 {
                if list[i].1[k] != list[j].1[k] {
                    eq = false;
                }
                k += 1;
            }
            if eq {
                panic!("ELEVATED_SPONSORED_GAS contains a duplicate target/selector pair");
            }
            j += 1;
        }
        i += 1;
    }
};

/// Maximum sum of included sponsored and one-time bootstrap transaction gas
/// limits in one block. Protocol fee waivers are outside this budget.
///
/// The target whitelist limits reachable code paths, not their block share.
/// This absolute policy limit must not scale with the block gas limit.
pub const SPONSORED_GAS_PER_BLOCK: u64 = 15_000_000;

const _: () = {
    assert!(SPONSORED_GAS_PER_BLOCK < STEADY_BLOCK_GAS_LIMIT);
    assert!(SPONSORED_GAS_PER_BLOCK >= FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT);
};

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
