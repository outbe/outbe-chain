//! Low-level ledger access for the Gratis token.
//!
//! This layer answers *how is Gratis storage read and mutated locally*:
//! metadata, balance/supply/pledge reads, and the internal balance-move
//! transition. Business orchestration (mint/burn/pledge/unpledge and event
//! emission) lives in [`crate::runtime`]; the cross-crate surface is
//! [`crate::api`].

use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::CREDIS_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};

use crate::schema::Gratis;

impl Gratis<'_> {
    // --- Metadata ---

    pub fn name(&self) -> &str {
        "gratis"
    }

    pub fn symbol(&self) -> &str {
        "GRATIS"
    }

    pub fn decimals(&self) -> u8 {
        18
    }

    // --- View functions ---

    pub fn total_supply(&self) -> Result<U256> {
        self.total_supply.read()
    }

    pub fn balance_of(&self, account: Address) -> Result<U256> {
        self.balances.read(&account)
    }

    /// Aggregate amount currently held in the credis escrow. Read directly
    /// from the balances map; no separate scalar is maintained.
    pub fn pledged_total_supply(&self) -> Result<U256> {
        self.balances.read(&CREDIS_ADDRESS)
    }

    /// Amount currently pledged by `account` and held in the credis escrow on
    /// their behalf. The sum of `pledged_of` across all accounts equals
    /// `pledged_total_supply()`.
    pub fn pledged_of(&self, account: Address) -> Result<U256> {
        self.pledged_balances.read(&account)
    }

    // --- Local state transitions ---

    /// Internal account-to-account transfer. No supply change. Not exposed via
    /// the precompile (gratis is non-transferable from user-land); reserved for
    /// the pledge/unpledge escrow moves in [`crate::runtime`].
    pub(crate) fn transfer_gratis(
        &mut self,
        from: Address,
        to: Address,
        amount: U256,
    ) -> Result<()> {
        if amount.is_zero() {
            return Err(PrecompileError::Revert("amount must be positive".into()));
        }
        if from.is_zero() || to.is_zero() {
            return Err(PrecompileError::Revert("invalid address".into()));
        }

        let from_balance = self.balances.read(&from)?;
        if from_balance < amount {
            return Err(PrecompileError::Revert("insufficient balance".into()));
        }

        let to_balance = self.balances.read(&to)?;
        let new_to_balance = checked_add(to_balance, amount, "gratis balance overflow")?;

        self.balances.write(&from, from_balance - amount)?;
        self.balances.write(&to, new_to_balance)?;

        Ok(())
    }
}

/// Overflow-checked `U256` addition for balance / supply accounting paths.
/// Raises a fatal precompile error on wrap instead of silently truncating.
pub(crate) fn checked_add(left: U256, right: U256, context: &'static str) -> Result<U256> {
    left.checked_add(right)
        .ok_or_else(|| PrecompileError::Revert(context.into()))
}
