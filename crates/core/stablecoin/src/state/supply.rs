use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_stablecoinpolicy::api::can_mint as policy_can_mint;

use crate::abi::IStablecoin;
use crate::errors::StablecoinStateError;
use crate::schema::{allowance_key, StablecoinContract, ISSUER_ROLE};

impl StablecoinContract<'_> {
    pub fn mint(&mut self, actor: Address, to: Address, amount: U256) -> Result<()> {
        self.require_role(ISSUER_ROLE, actor)?;
        self.require_not_paused()?;
        self.require_nonzero(to)?;
        self.require_can_mint(to)?;
        self.mint_core(to, amount)
    }

    pub fn burn(&mut self, actor: Address, amount: U256) -> Result<()> {
        self.require_role(ISSUER_ROLE, actor)?;
        self.require_not_paused()?;
        self.require_nonzero_amount(amount)?;
        let balance = self.balances.read(&actor)?;
        if balance < amount {
            return Err(StablecoinStateError::InsufficientBalance {
                account: actor,
                available: balance,
                required: amount,
            }
            .into());
        }
        let frozen = self.frozen.read(&actor)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.consume_frozen(actor, balance, frozen, amount)?;
            self.burn_core(actor, amount)
        })
    }

    pub fn burn_from(&mut self, spender: Address, from: Address, amount: U256) -> Result<()> {
        self.validated_schema_version()?;
        self.require_not_paused()?;
        self.require_nonzero(spender)?;
        self.require_nonzero(from)?;
        self.require_nonzero_amount(amount)?;
        self.require_can_send(from)?;

        let key = allowance_key(from, spender);
        let allowance = self.allowances.read(&key)?;
        if allowance < amount {
            return Err(StablecoinStateError::InsufficientAllowance {
                owner: from,
                spender,
                available: allowance,
                required: amount,
            }
            .into());
        }
        let balance = self.balances.read(&from)?;
        if balance < amount {
            return Err(StablecoinStateError::InsufficientBalance {
                account: from,
                available: balance,
                required: amount,
            }
            .into());
        }
        self.require_unfrozen(from, balance, amount)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.burn_core(from, amount)?;
            if allowance != U256::MAX {
                self.allowances.write(&key, allowance - amount)?;
            }
            Ok(())
        })
    }

    pub fn forced_transfer(
        &mut self,
        actor: Address,
        from: Address,
        to: Address,
        amount: U256,
    ) -> Result<()> {
        self.require_role(crate::schema::ENFORCER_ROLE, actor)?;
        self.require_not_paused()?;
        self.require_nonzero(from)?;
        self.require_nonzero(to)?;
        self.require_nonzero_amount(amount)?;
        if from == to {
            return Err(StablecoinStateError::ForcedSelfTransfer.into());
        }
        self.require_can_receive(to)?;
        let from_balance = self.balances.read(&from)?;
        if from_balance < amount {
            return Err(StablecoinStateError::InsufficientBalance {
                account: from,
                available: from_balance,
                required: amount,
            }
            .into());
        }
        let to_balance = self.balances.read(&to)?;
        let next_to = to_balance.checked_add(amount).ok_or_else(|| {
            PrecompileError::Fatal("stablecoin forced-transfer recipient overflow".into())
        })?;
        let frozen = self.frozen.read(&from)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.consume_frozen(from, from_balance, frozen, amount)?;
            self.balances.write(&from, from_balance - amount)?;
            self.balances.write(&to, next_to)?;
            self.emit(IStablecoin::Transfer {
                from,
                to,
                value: amount,
            })?;
            self.emit(IStablecoin::ForcedTransfer { from, to, amount })
        })?;
        Ok(())
    }

    pub fn mint_with_memo(
        &mut self,
        actor: Address,
        to: Address,
        amount: U256,
        memo: B256,
    ) -> Result<()> {
        self.with_memo(Address::ZERO, to, amount, memo, |token| {
            token.mint(actor, to, amount)
        })
    }

    pub fn burn_with_memo(&mut self, actor: Address, amount: U256, memo: B256) -> Result<()> {
        self.with_memo(actor, Address::ZERO, amount, memo, |token| {
            token.burn(actor, amount)
        })
    }

    pub fn burn_from_with_memo(
        &mut self,
        spender: Address,
        from: Address,
        amount: U256,
        memo: B256,
    ) -> Result<()> {
        self.with_memo(from, Address::ZERO, amount, memo, |token| {
            token.burn_from(spender, from, amount)
        })
    }

    pub fn forced_transfer_with_memo(
        &mut self,
        actor: Address,
        from: Address,
        to: Address,
        amount: U256,
        memo: B256,
    ) -> Result<()> {
        self.with_memo(from, to, amount, memo, |token| {
            token.forced_transfer(actor, from, to, amount)
        })
    }

    pub(crate) fn mint_core(&mut self, to: Address, amount: U256) -> Result<()> {
        self.validated_schema_version()?;
        self.require_nonzero(to)?;
        self.require_nonzero_amount(amount)?;

        let supply = self.total_supply.read()?;
        let supply_cap = self.supply_cap.read()?;
        if supply > supply_cap {
            return Err(PrecompileError::Fatal(
                "stablecoin total supply exceeds its stored cap".into(),
            ));
        }
        let requested_supply = supply.checked_add(amount).ok_or_else(|| {
            PrecompileError::from(StablecoinStateError::SupplyCapExceeded {
                supply_cap,
                requested_supply: U256::MAX,
            })
        })?;
        if requested_supply > supply_cap {
            return Err(StablecoinStateError::SupplyCapExceeded {
                supply_cap,
                requested_supply,
            }
            .into());
        }
        let balance = self.balances.read(&to)?;
        let next_balance = balance
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Fatal("stablecoin balance overflow".into()))?;

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.total_supply.write(requested_supply)?;
            self.balances.write(&to, next_balance)?;
            self.emit(IStablecoin::Transfer {
                from: Address::ZERO,
                to,
                value: amount,
            })
        })
    }

    pub(crate) fn burn_core(&mut self, from: Address, amount: U256) -> Result<()> {
        self.validated_schema_version()?;
        self.require_nonzero(from)?;
        self.require_nonzero_amount(amount)?;
        let balance = self.balances.read(&from)?;
        if balance < amount {
            return Err(StablecoinStateError::InsufficientBalance {
                account: from,
                available: balance,
                required: amount,
            }
            .into());
        }
        let supply = self.total_supply.read()?;
        let next_supply = supply.checked_sub(amount).ok_or_else(|| {
            PrecompileError::Fatal("stablecoin supply/balance invariant is corrupted".into())
        })?;

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.balances.write(&from, balance - amount)?;
            self.total_supply.write(next_supply)?;
            self.emit(IStablecoin::Transfer {
                from,
                to: Address::ZERO,
                value: amount,
            })
        })
    }

    fn require_can_mint(&self, account: Address) -> Result<()> {
        let policy_id = self.policy_id.read()?;
        if !policy_can_mint(self.storage.clone(), policy_id, account)? {
            return Err(StablecoinStateError::ERC7943CannotReceive { account }.into());
        }
        Ok(())
    }
}
