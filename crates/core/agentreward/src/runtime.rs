use crate::schema::AgentRewardContract;
use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::error::{PrecompileError, Result};

impl AgentRewardContract<'_> {
    /// Increments WAA (wallet) tribute count for an address on `day`.
    ///
    /// On the first tribute for this address+day pair, the address is
    /// also appended to the per-day WAA address list so it can be
    /// enumerated during distribution.
    pub fn increment_waa_tribute(&mut self, day: WorldwideDay, address: Address) -> Result<()> {
        let key = AgentRewardContract::tribute_count_key(day, address);
        let count = self.waa_tribute_counts.read(&key)?;
        if count == 0 {
            // First tribute for this address+day — add to address list
            let addr_count = self.waa_address_count.read(&day)?;
            let idx_key = AgentRewardContract::address_index_key(day, addr_count);
            self.waa_addresses.write(&idx_key, address)?;
            self.waa_address_count.write(&day, addr_count + 1)?;
        }
        self.waa_tribute_counts.write(&key, count + 1)
    }

    /// Increments SRA tribute count for an address on `day`.
    ///
    /// On the first tribute for this address+day pair, the address is
    /// also appended to the per-day SRA address list so it can be
    /// enumerated during distribution.
    pub fn increment_sra_tribute(&mut self, day: WorldwideDay, address: Address) -> Result<()> {
        let key = AgentRewardContract::tribute_count_key(day, address);
        let count = self.sra_tribute_counts.read(&key)?;
        if count == 0 {
            // First tribute for this address+day — add to address list
            let addr_count = self.sra_address_count.read(&day)?;
            let idx_key = AgentRewardContract::address_index_key(day, addr_count);
            self.sra_addresses.write(&idx_key, address)?;
            self.sra_address_count.write(&day, addr_count + 1)?;
        }
        self.sra_tribute_counts.write(&key, count + 1)
    }

    /// Gets claimable reward balance for an address.
    pub fn get_claimable_reward(&self, address: Address) -> Result<U256> {
        self.claimable_rewards.read(&address)
    }

    /// Adds amount to claimable reward for an address.
    pub fn add_claimable_reward(&mut self, address: Address, amount: U256) -> Result<()> {
        let current = self.claimable_rewards.read(&address)?;
        self.claimable_rewards.write(
            &address,
            checked_add(current, amount, "agentreward claimable_rewards overflow")?,
        )
    }

    /// Claims reward: subtracts amount from claimable balance and transfers
    /// real native tokens from the contract address to the caller.
    pub fn claim_reward(&mut self, address: Address, amount: U256) -> Result<U256> {
        let balance = self.claimable_rewards.read(&address)?;
        if amount > balance {
            return Err(outbe_primitives::error::PrecompileError::Revert(
                "insufficient claimable balance".into(),
            ));
        }
        self.claimable_rewards.write(&address, balance - amount)?;

        // Transfer real tokens from contract to claimant.
        self.storage.transfer_balance(
            outbe_primitives::addresses::AGENT_REWARD_ADDRESS,
            address,
            amount,
        )?;

        Ok(amount)
    }

    /// Gets all WAA tribute counts for a day as (address, count) pairs.
    pub fn get_all_waa_counts(&self, day: WorldwideDay) -> Result<Vec<(Address, u64)>> {
        let addr_count = self.waa_address_count.read(&day)?;
        let mut result = Vec::with_capacity(addr_count as usize);
        for i in 0..addr_count {
            let idx_key = AgentRewardContract::address_index_key(day, i);
            let addr = self.waa_addresses.read(&idx_key)?;
            if addr.is_zero() {
                continue;
            }
            let count_key = AgentRewardContract::tribute_count_key(day, addr);
            let count = self.waa_tribute_counts.read(&count_key)?;
            if count > 0 {
                result.push((addr, count));
            }
        }
        Ok(result)
    }

    /// Gets all SRA tribute counts for a day as (address, count) pairs.
    pub fn get_all_sra_counts(&self, day: WorldwideDay) -> Result<Vec<(Address, u64)>> {
        let addr_count = self.sra_address_count.read(&day)?;
        let mut result = Vec::with_capacity(addr_count as usize);
        for i in 0..addr_count {
            let idx_key = AgentRewardContract::address_index_key(day, i);
            let addr = self.sra_addresses.read(&idx_key)?;
            if addr.is_zero() {
                continue;
            }
            let count_key = AgentRewardContract::tribute_count_key(day, addr);
            let count = self.sra_tribute_counts.read(&count_key)?;
            if count > 0 {
                result.push((addr, count));
            }
        }
        Ok(result)
    }

    /// Clears WAA tribute counts and address list for a day. Called from
    /// the distribution path once the day's WAA pool has been settled.
    pub fn clear_waa_counts(&mut self, day: WorldwideDay) -> Result<()> {
        let waa_count = self.waa_address_count.read(&day)?;
        for i in 0..waa_count {
            let idx_key = AgentRewardContract::address_index_key(day, i);
            let addr = self.waa_addresses.read(&idx_key)?;
            if !addr.is_zero() {
                let count_key = AgentRewardContract::tribute_count_key(day, addr);
                self.waa_tribute_counts.write(&count_key, 0)?;
                self.waa_addresses.write(&idx_key, Address::ZERO)?;
            }
        }
        self.waa_address_count.write(&day, 0)?;
        Ok(())
    }

    /// Clears SRA tribute counts and address list for a day. Called from
    /// the distribution path once the day's SRA pool has been settled.
    pub fn clear_sra_counts(&mut self, day: WorldwideDay) -> Result<()> {
        let sra_count = self.sra_address_count.read(&day)?;
        for i in 0..sra_count {
            let idx_key = AgentRewardContract::address_index_key(day, i);
            let addr = self.sra_addresses.read(&idx_key)?;
            if !addr.is_zero() {
                let count_key = AgentRewardContract::tribute_count_key(day, addr);
                self.sra_tribute_counts.write(&count_key, 0)?;
                self.sra_addresses.write(&idx_key, Address::ZERO)?;
            }
        }
        self.sra_address_count.write(&day, 0)?;
        Ok(())
    }
}

/// Overflow-checked `U256` addition for reward accounting paths.
fn checked_add(left: U256, right: U256, context: &'static str) -> Result<U256> {
    left.checked_add(right)
        .ok_or_else(|| PrecompileError::Revert(context.into()))
}
