//! Gas model for Outbe stateful precompiles.
//!
//! This file prices storage access for a stateful precompile.
//! A read uses the revm warm read cost
//! [`WARM_STORAGE_READ_COST`](revm::context_interface::cfg::gas::WARM_STORAGE_READ_COST).
//! A write uses the revm reset cost
//! [`SSTORE_RESET`](revm::context_interface::cfg::gas::SSTORE_RESET).
//! These two costs do not change when the block base fee changes.
//! A precompile storage access is not an EIP-2929 access.
//! This file has no warm/cold split and no SSTORE refund.
//!
//! `CtxStorageProvider` in `crates/blockchain/evm/src/storage/ctx_provider.rs`
//! is the production storage provider. It applies these two costs on the
//! precompile dispatch path.
//!
//! A permissioned validator set produces blocks.
//! The node still computes an EIP-1559 base fee.
//! Header checks call `validate_against_parent_eip1559_base_fee`
//! in `crates/blockchain/node/src/consensus/policy.rs`.
//! The base fee applies to ordinary EVM execution and to transaction order.
//! The base fee does not scale the constants in this file.
//!
//! The Outbe-specific constants live here: [`PRECOMPILE_BASE_GAS`] for flat
//! dispatch entry and [`ZK_VERIFY_GAS`] for one proof verification.

/// Flat entry cost charged once per precompile dispatch.
///
/// No direct EIP-2929 counterpart: a precompile entry is a cross-boundary
/// action, not an SLOAD. The value is in the same order of magnitude as the
/// cheapest metered op, so dispatch is cheap but not free. This is adequate
/// DoS protection in Outbe's permissioned model.
pub const PRECOMPILE_BASE_GAS: u64 = 200;

/// Flat gas cost for one UltraHonkKeccak verification. This is the protocol
/// charge for verification. Calldata and storage gas are additional.
///
/// Rationale for the value:
///
/// - The charge is flat. The circuit sets the UltraHonkKeccak verification
///   cost, not the input size. Transaction calldata gas already bills the
///   proof bytes.
/// - Commit `fd16ab63` (#408) lowered the value from 3_000_000 to 300_000
///   after release measurements.
/// - The Tribute scenario in `testing/protocol-benchmarks` is the measurement
///   to run again before you change this value. `measure_verify_ms` measures
///   the verifier latency. `baselines/gas-v1.json` pins the
///   `precompile.configured_base` component at 300000.
/// - Outbe runs a permissioned validator set with a 30M block gas limit. This
///   limits a block to approximately 100 verifications.
pub const ZK_VERIFY_GAS: u64 = 300_000;

#[cfg(test)]
mod tests {
    use super::*;
    use revm::context_interface::cfg::gas::{SSTORE_RESET, WARM_STORAGE_READ_COST};

    #[test]
    fn outbe_protocol_gas_values_are_stable() {
        // Protocol-visible values. If revm ever changes these upstream,
        // Outbe must decide explicitly whether to follow or pin a local
        // value. A silent upstream shift is a protocol change.
        assert_eq!(WARM_STORAGE_READ_COST, 100);
        assert_eq!(SSTORE_RESET, 5_000);
        assert_eq!(PRECOMPILE_BASE_GAS, 200);
    }
}
