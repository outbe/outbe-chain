//! Business logic for the Gratis token: mint, burn, and the pledge/unpledge
//! escrow lifecycle.
//!
//! Every entry point here validates its inputs, mutates the ledger through
//! [`crate::state`], and emits the matching `IGratis` event. These methods are
//! crate-private; other crates reach them through the curated [`crate::api`]
//! surface, and the EVM precompile ABI routes through [`crate::precompile`].

use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::CREDIS_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};

use crate::precompile::IGratis;
use crate::schema::Gratis;
use crate::state::checked_add;

impl Gratis<'_> {
    /// Mints gratis tokens to an account. Reached from [`crate::api::mine`];
    /// gratisfactory is the production caller.
    pub(crate) fn mine(&mut self, account: Address, amount: U256) -> Result<U256> {
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

    /// Burns gratis tokens from an account. Reached from [`crate::api::burn`].
    pub(crate) fn burn(&mut self, account: Address, amount: U256) -> Result<U256> {
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

    /// Locks `amount` gratis from `account` into the credis escrow.
    ///
    /// Moves the balance from `account` to `CREDIS_ADDRESS` and increments
    /// the per-account pledge ledger. `total_supply` is unchanged (pledged
    /// gratis is still circulating supply, just not user-spendable). The
    /// aggregate pledged amount is `balances[CREDIS_ADDRESS]`.
    pub(crate) fn pledge(&mut self, account: Address, amount: U256) -> Result<U256> {
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
    pub(crate) fn unpledge(&mut self, account: Address, amount: U256) -> Result<U256> {
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
