use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};

use crate::abi::IStablecoin;
use crate::errors::StablecoinStateError;
use crate::schema::StablecoinContract;

impl StablecoinContract<'_> {
    pub fn frozen_tokens(&self, account: Address) -> Result<U256> {
        self.validated_schema_version()?;
        self.frozen.read(&account)
    }

    pub fn set_frozen_tokens(
        &mut self,
        actor: Address,
        account: Address,
        amount: U256,
    ) -> Result<()> {
        self.require_role(crate::schema::COMPLIANCE_ROLE, actor)?;
        self.require_nonzero(account)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.frozen.write(&account, amount)?;
            self.emit(IStablecoin::Frozen { account, amount })
        })
    }

    pub(super) fn require_unfrozen(
        &self,
        account: Address,
        balance: U256,
        amount: U256,
    ) -> Result<()> {
        let available = Self::unfrozen(balance, self.frozen.read(&account)?);
        if amount > available {
            return Err(StablecoinStateError::ERC7943InsufficientUnfrozenBalance {
                account,
                amount,
                unfrozen: available,
            }
            .into());
        }
        Ok(())
    }

    pub(super) fn consume_frozen(
        &mut self,
        account: Address,
        balance: U256,
        frozen: U256,
        amount: U256,
    ) -> Result<()> {
        let available = Self::unfrozen(balance, frozen);
        let consumed = if amount > available {
            amount - available
        } else {
            U256::ZERO
        };
        if consumed.is_zero() {
            return Ok(());
        }
        let next_frozen = frozen.checked_sub(consumed).ok_or_else(|| {
            PrecompileError::Fatal("stablecoin frozen-balance invariant is corrupted".into())
        })?;
        self.frozen.write(&account, next_frozen)?;
        self.emit(IStablecoin::Frozen {
            account,
            amount: next_frozen,
        })
    }
}
