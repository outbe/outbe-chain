use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};

use crate::precompile::IPromis;
use crate::schema::Promis;

impl Promis<'_> {
    // --- View functions ---

    pub fn name(&self) -> &str {
        "promis"
    }

    pub fn symbol(&self) -> &str {
        "PROMIS"
    }

    pub fn decimals(&self) -> u8 {
        18
    }

    pub fn total_supply(&self) -> Result<U256> {
        self.total_supply.read()
    }

    pub fn balance_of(&self, account: Address) -> Result<U256> {
        self.balances.read(&account)
    }

    // --- State-changing functions ---

    /// Mints promis tokens to an account.
    pub fn mint(&mut self, account: Address, amount: U256) -> Result<U256> {
        if amount.is_zero() {
            return Err(PrecompileError::Revert("amount must be positive".into()));
        }
        if account.is_zero() {
            return Err(PrecompileError::Revert("invalid address".into()));
        }

        // Compute both updated values before any write so a late overflow does
        // not leave balance/supply desynchronised.
        let balance = self.balances.read(&account)?;
        let new_balance = checked_add(balance, amount, "promis balance overflow")?;

        let supply = self.total_supply.read()?;
        let new_supply = checked_add(supply, amount, "promis total_supply overflow")?;

        self.balances.write(&account, new_balance)?;
        self.total_supply.write(new_supply)?;

        self.emit(IPromis::PromisMinted {
            account,
            amount,
            newTotalSupply: new_supply,
        })?;

        Ok(new_supply)
    }

    /// Burns promis tokens from an account.
    pub fn burn(&mut self, account: Address, amount: U256) -> Result<U256> {
        if amount.is_zero() {
            return Err(PrecompileError::Revert("amount must be positive".into()));
        }

        let balance = self.balances.read(&account)?;
        if balance < amount {
            return Err(PrecompileError::Revert("insufficient balance".into()));
        }

        self.balances.write(&account, balance - amount)?;

        let supply = self.total_supply.read()?;
        let remaining = supply - amount;
        self.total_supply.write(remaining)?;

        self.emit(IPromis::PromisBurned {
            account,
            amount,
            remainingSupply: remaining,
        })?;

        Ok(remaining)
    }
}

/// Overflow-checked `U256` addition for balance / supply accounting paths.
fn checked_add(left: U256, right: U256, context: &'static str) -> Result<U256> {
    left.checked_add(right)
        .ok_or_else(|| PrecompileError::Revert(context.into()))
}
