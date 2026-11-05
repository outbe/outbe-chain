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
//! applies these two costs on the transaction precompile dispatch path.
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

/// Invalid use of the checkpoint-owning running trigger scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TriggerScopeError {
    #[error("provider does not support metered trigger scopes")]
    Unsupported,
    #[error("nested trigger scopes are not supported")]
    Nested,
    #[error("no trigger scope is active")]
    Inactive,
    #[error("invalid unused gas return")]
    InvalidGasReturn,
}

/// Gross work allowance. This is deliberately not part of checkpoint state.
#[derive(Debug, Clone)]
pub struct TriggerBudget {
    pub remaining: u64,
    budget: u64,
    exhausted: bool,
    infrastructure_failure: Option<String>,
}

impl TriggerBudget {
    pub const fn new(budget: u64) -> Self {
        Self {
            remaining: budget,
            budget,
            exhausted: false,
            infrastructure_failure: None,
        }
    }

    pub fn status(&self) -> crate::error::Result<()> {
        if let Some(reason) = &self.infrastructure_failure {
            return Err(crate::error::PrecompileError::Fatal(reason.clone()));
        }
        if self.exhausted {
            Err(crate::error::PrecompileError::TriggerBudgetExceeded {
                consumed_gas: self.budget - self.remaining,
                budget_gas: self.budget,
            })
        } else {
            Ok(())
        }
    }

    pub fn charge(&mut self, cost: u64) -> crate::error::Result<()> {
        self.check(cost)?;
        self.remaining -= cost;
        Ok(())
    }

    pub fn check(&mut self, cost: u64) -> crate::error::Result<()> {
        self.status()?;
        if cost > self.remaining {
            self.exhausted = true;
            return self.status();
        }
        Ok(())
    }

    pub fn exhaust(&mut self) {
        self.exhausted = true;
    }

    /// A caller cannot turn a caught backend failure into successful deferral.
    pub fn observe_error(&mut self, error: &crate::error::PrecompileError) {
        use crate::error::PrecompileError;
        if self.infrastructure_failure.is_none()
            && matches!(
                error,
                PrecompileError::Storage(_)
                    | PrecompileError::BodyReadCorruption(_)
                    | PrecompileError::Fatal(_)
                    | PrecompileError::FatalWithCause(_)
                    | PrecompileError::Unsupported
                    | PrecompileError::EnclaveUnavailable(_)
                    | PrecompileError::SubCall(_)
            )
        {
            self.infrastructure_failure = Some(error.to_string());
        }
    }

    pub fn return_unused(&mut self, gas: u64) -> crate::error::Result<()> {
        self.remaining = self
            .remaining
            .checked_add(gas)
            .filter(|remaining| *remaining <= self.budget)
            .ok_or(TriggerScopeError::InvalidGasReturn)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use revm::context_interface::cfg::gas::{SSTORE_RESET, WARM_STORAGE_READ_COST};

    #[test]
    fn unused_return_cannot_create_capacity_or_overflow() {
        for budget in [100, u64::MAX] {
            let mut meter = TriggerBudget::new(budget);
            meter.charge(100).unwrap();
            meter.return_unused(100).unwrap();
            assert!(matches!(
                meter.return_unused(1),
                Err(crate::error::PrecompileError::TriggerScope(
                    TriggerScopeError::InvalidGasReturn
                ))
            ));
            assert_eq!(meter.remaining, budget);
        }
    }

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
