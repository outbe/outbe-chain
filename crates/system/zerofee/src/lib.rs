//! System-level zero-fee transaction policy registry.
//!
//! Zero-fee transactions are still normal signed EVM transactions. This crate
//! owns the deterministic policy hooks that decide whether a specific public
//! transaction class may skip native fee debit. Execution and txpool crates only
//! adapt their transaction/state views into this registry.
//!
//! ## Two parallel mechanisms
//!
//! 1. **Trait-registry hooks** (`hooks.rs`, `oracle.rs`): stateless envelope
//!    classification + stateful authorization for protocol-defined
//!    transactions like `Oracle.submitVote`. The signer is authenticated by
//!    cross-module state (e.g. validator status).
//!
//! 2. **EIP-7702 paymaster precompile** (`schema.rs`, `runtime.rs`,
//!    `precompile.rs`): a stateful precompile at
//!    [`outbe_primitives::addresses::ZEROFEE_ADDRESS`]. It grants up to
//!    `FREE_TX_DAILY_LIMIT` free transactions per UTC day to every EOA that
//!    has delegated to it (via an EIP-7702 set-code authorization). The executor's
//!    pre-fee path detects the delegation designator and routes the
//!    transaction through the same `disable_balance_check`,
//!    `disable_base_fee`, and `disable_fee_charge` machinery that already
//!    serves oracle votes.
//!
//! Sponsorship authorization returns an opaque signer/day token without writing state.
//! The executor consumes it only after inclusion, including an included revert.
//! Authorization failures, execution errors, and declined commits do not spend quota.
//! This protects the typed Rust API and storage schema, not raw storage writes.
//!
//! External crates cannot construct a sponsorship grant, pass a bare day to
//! the recorder, or write the quota map. The three examples below are the
//! compile-fail boundary for that interface.
//!
//! ```compile_fail
//! fn forged() -> outbe_zerofee::SponsorshipAuthorization {
//!     outbe_zerofee::SponsorshipAuthorization {
//!         signer: alloy_primitives::Address::ZERO,
//!         current_day: 1,
//!         next_count: 1,
//!     }
//! }
//! ```
//!
//! ```compile_fail
//! fn bare_day(
//!     storage: outbe_primitives::storage::StorageHandle<'_>,
//!     signer: alloy_primitives::Address,
//! ) {
//!     let _ = outbe_zerofee::record_sponsorship_use(storage, signer, 1u32);
//! }
//! ```
//!
//! ```compile_fail
//! fn write_counter(contract: &outbe_zerofee::ZeroFeeContract<'_>) {
//!     let _ = contract.counter.write(&alloy_primitives::Address::ZERO, 0u64);
//! }
//! ```

pub mod constants;
mod envelope;
pub mod errors;
#[cfg(any(test, feature = "test-utils"))]
pub mod fixtures;
#[cfg(test)]
mod hook_registry_tests;
pub mod hooks;
mod hyperlane;
mod oracle;
pub mod precompile;
mod renewal;
pub mod runtime;
pub mod schema;
#[cfg(test)]
mod test_support;
mod transaction;

pub use constants::{
    FREE_TX_BOOTSTRAP_GAS_LIMIT, FREE_TX_DAILY_CALLDATA_BYTES, FREE_TX_DAILY_GAS_LIMIT,
    FREE_TX_DAILY_LIMIT, FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT, MIN_FREE_TX_MAX_FEE_PER_GAS,
    SPONSORED_GAS_PER_BLOCK,
};
pub use hooks::{
    registry, ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeeHook, ZeroFeeHookId,
    ZeroFeePolicyError, ZeroFeeRegistry, ZeroFeeTransaction,
};
pub use outbe_primitives::addresses::ZEROFEE_ADDRESS;
pub use runtime::{
    authorize_bootstrap, authorize_sponsorship, classify_bootstrap, classify_sponsorship,
    precheck_sponsorship, record_sponsorship_use, BootstrapAccountView, BootstrapCandidate,
    BootstrapTransactionView, SponsorshipAuthorization,
};
pub use schema::{pack_counter, unpack_counter, ZeroFeeContract};

/// Whether including `gas_limit` would exceed the sponsored block budget.
///
/// `used` is the sum of gas limits of included sponsored and bootstrap
/// transactions, not their actual gas consumption. Overflow is over budget.
pub fn sponsored_gas_exceeds_block_budget(used: u64, gas_limit: u64) -> bool {
    match used.checked_add(gas_limit) {
        Some(total) => total > SPONSORED_GAS_PER_BLOCK,
        None => true,
    }
}

#[cfg(test)]
mod sponsored_budget_tests {
    use super::*;

    #[test]
    fn sponsored_budget_boundaries() {
        for (used, gas_limit, exceeds) in [
            (0, 0, false),
            (0, FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT, false),
            (0, SPONSORED_GAS_PER_BLOCK, false),
            (SPONSORED_GAS_PER_BLOCK - 1, 1, false),
            (SPONSORED_GAS_PER_BLOCK, 0, false),
            (SPONSORED_GAS_PER_BLOCK, 1, true),
            (0, SPONSORED_GAS_PER_BLOCK + 1, true),
            (13_000_000, FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT, true),
            (13_000_000, FREE_TX_DAILY_GAS_LIMIT, false),
            (SPONSORED_GAS_PER_BLOCK + 1, 0, true),
        ] {
            assert_eq!(
                sponsored_gas_exceeds_block_budget(used, gas_limit),
                exceeds,
                "used={used}, gas_limit={gas_limit}"
            );
        }
    }

    #[test]
    fn sponsored_budget_rejects_overflow() {
        for (used, gas_limit) in [(u64::MAX, 1), (1, u64::MAX), (u64::MAX, u64::MAX)] {
            assert!(sponsored_gas_exceeds_block_budget(used, gas_limit));
        }
    }

    #[test]
    fn sponsored_budget_is_absolute_and_within_bounds() {
        assert_eq!(SPONSORED_GAS_PER_BLOCK, 15_000_000);
        assert!((FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT
            ..outbe_primitives::system_tx::STEADY_BLOCK_GAS_LIMIT)
            .contains(&SPONSORED_GAS_PER_BLOCK));
    }
}
