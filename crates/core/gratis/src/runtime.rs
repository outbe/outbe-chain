use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::CREDIS_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};

use crate::precompile::IGratis;
use crate::schema::Gratis;

impl Gratis<'_> {
    // --- View functions ---

    pub fn name(&self) -> &str {
        "gratis"
    }

    pub fn symbol(&self) -> &str {
        "GRATIS"
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

    // --- State-changing functions ---

    /// Mints gratis tokens to an account. Internal Rust API; not exposed via
    /// the precompile. Lysis is the production caller.
    pub fn mine(&mut self, account: Address, amount: U256) -> Result<U256> {
        if amount.is_zero() {
            return Err(PrecompileError::Revert("amount must be positive".into()));
        }
        if account.is_zero() {
            return Err(PrecompileError::Revert("invalid address".into()));
        }

        // Compute both updated values before any write so a late overflow does
        // not leave balance/supply desynchronised.
        let balance = self.balances.read(&account)?;
        let new_balance = checked_add(balance, amount, "gratis balance overflow")?;

        let supply = self.total_supply.read()?;
        let new_supply = checked_add(supply, amount, "gratis total_supply overflow")?;

        self.balances.write(&account, new_balance)?;
        self.total_supply.write(new_supply)?;

        self.emit(IGratis::GratisMined {
            account,
            amount,
            newTotalSupply: new_supply,
        })?;

        Ok(new_supply)
    }

    /// Burns gratis tokens from an account. Internal Rust API.
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

        self.emit(IGratis::GratisBurned {
            account,
            amount,
            remainingSupply: remaining,
        })?;

        Ok(remaining)
    }

    /// Internal account-to-account transfer. No supply change. Not exposed via
    /// the precompile (gratis is non-transferable from user-land);
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

    /// Locks `amount` gratis from `account` into the credis escrow.
    ///
    /// Moves the balance from `account` to `CREDIS_ADDRESS` and increments
    /// the per-account pledge ledger. `total_supply` is unchanged (pledged
    /// gratis is still circulating supply, just not user-spendable). The
    /// aggregate pledged amount is `balances[CREDIS_ADDRESS]`.
    pub fn pledge(&mut self, account: Address, amount: U256) -> Result<U256> {
        if amount.is_zero() {
            return Err(PrecompileError::Revert("amount must be positive".into()));
        }

        // Compute the new per-account pledged balance up front so a late
        // overflow does not leave the escrow transfer half-applied.
        let account_pledged = self.pledged_balances.read(&account)?;
        let new_account_pledged =
            checked_add(account_pledged, amount, "gratis pledged_balances overflow")?;

        // transfer_gratis enforces from-balance ≥ amount and validates addrs.
        self.transfer_gratis(account, CREDIS_ADDRESS, amount)?;
        self.pledged_balances.write(&account, new_account_pledged)?;

        let total_pledged = self.balances.read(&CREDIS_ADDRESS)?;
        self.emit(IGratis::GratisPledged {
            account,
            amount,
            totalPledged: total_pledged,
        })?;

        Ok(total_pledged)
    }

    /// Releases `amount` gratis from the credis escrow back to `account`.
    ///
    /// Reverts unless `account` itself has at least `amount` recorded in the
    /// per-account pledge ledger; on success moves the balance from
    /// `CREDIS_ADDRESS` to `account` and decrements the ledger. The
    /// per-account ledger is the binding check: under the invariant
    /// `sum(pledged_balances) == balances[CREDIS_ADDRESS]` this also
    /// guarantees the escrow has enough balance to cover the transfer.
    pub fn unpledge(&mut self, account: Address, amount: U256) -> Result<U256> {
        if amount.is_zero() {
            return Err(PrecompileError::Revert("amount must be positive".into()));
        }

        let account_pledged = self.pledged_balances.read(&account)?;
        if account_pledged < amount {
            return Err(PrecompileError::Revert(
                "insufficient pledged balance".into(),
            ));
        }

        self.transfer_gratis(CREDIS_ADDRESS, account, amount)?;
        self.pledged_balances
            .write(&account, account_pledged - amount)?;

        let remaining_pledged = self.balances.read(&CREDIS_ADDRESS)?;
        self.emit(IGratis::GratisUnpledged {
            account,
            amount,
            remainingPledged: remaining_pledged,
        })?;

        Ok(remaining_pledged)
    }

}

/// Overflow-checked `U256` addition for balance / supply accounting paths.
/// Raises a fatal precompile error on wrap instead of silently truncating.
fn checked_add(left: U256, right: U256, context: &'static str) -> Result<U256> {
    left.checked_add(right)
        .ok_or_else(|| PrecompileError::Revert(context.into()))
}
