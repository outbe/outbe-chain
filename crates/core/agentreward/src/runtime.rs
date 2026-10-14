use crate::errors::AgentRewardError;
use crate::precompile::IAgentReward;
use crate::schema::{AgentRewardContract, RewardPool};
use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{ExecutionReaders, ParentBodySource};
use outbe_gemfactory::schema::GemTypes;
use outbe_primitives::error::Result;
use outbe_primitives::storage::dsl::Map;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key, WorldwideDay};
use outbe_primitives::units::{checked_protocol_to_native, native_to_protocol_floor};

impl AgentRewardContract<'_> {
    /// Increments WAA (wallet) tribute count for an address on `day`.
    ///
    /// On the first tribute for this address+day pair, the function also
    /// appends the address to the per-day WAA address list. Distribution
    /// enumerates that list.
    pub fn increment_waa_tribute(&mut self, day: WorldwideDay, address: Address) -> Result<()> {
        let key = AgentRewardContract::tribute_count_key(day, address);
        let count = self.waa_tribute_counts.read(&key)?;
        if count == 0 {
            // First tribute for this address+day - add to address list
            let addr_count = self.waa_address_count.read(&day)?;
            let idx_key = AgentRewardContract::address_index_key(day, addr_count);
            self.waa_addresses.write(&idx_key, address)?;
            self.waa_address_count.write(&day, addr_count + 1)?;
        }
        self.waa_tribute_counts.write(&key, count + 1)
    }

    /// Increments SRA tribute count for an address on `day`.
    ///
    /// On the first tribute for this address+day pair, the function also
    /// appends the address to the per-day SRA address list. Distribution
    /// enumerates that list.
    pub fn increment_sra_tribute(&mut self, day: WorldwideDay, address: Address) -> Result<()> {
        let key = AgentRewardContract::tribute_count_key(day, address);
        let count = self.sra_tribute_counts.read(&key)?;
        if count == 0 {
            // First tribute for this address+day - add to address list
            let addr_count = self.sra_address_count.read(&day)?;
            let idx_key = AgentRewardContract::address_index_key(day, addr_count);
            self.sra_addresses.write(&idx_key, address)?;
            self.sra_address_count.write(&day, addr_count + 1)?;
        }
        self.sra_tribute_counts.write(&key, count + 1)
    }

    /// Gets the total claimable reward balance for an address across all three pools.
    pub fn get_claimable_reward(&self, address: Address) -> Result<U256> {
        let waa = self.get_pool_claimable_reward(RewardPool::Waa, address)?;
        let sra = self.get_pool_claimable_reward(RewardPool::Sra, address)?;
        let cca = self.get_pool_claimable_reward(RewardPool::Cca, address)?;
        waa.checked_add(sra)
            .and_then(|subtotal| subtotal.checked_add(cca))
            .ok_or_else(|| AgentRewardError::ClaimableTotalOverflow.into())
    }

    /// Gets the claimable reward balance an address holds in one pool.
    pub fn get_pool_claimable_reward(&self, pool: RewardPool, address: Address) -> Result<U256> {
        match pool {
            RewardPool::Waa => self.waa_claimable_rewards.read(&address),
            RewardPool::Sra => self.sra_claimable_rewards.read(&address),
            RewardPool::Cca => self.cca_claimable_rewards.read(&address),
        }
    }

    /// Adds amount to an address's claimable reward in one pool.
    pub fn add_claimable_reward(
        &mut self,
        pool: RewardPool,
        address: Address,
        amount: U256,
    ) -> Result<()> {
        let current = self.get_pool_claimable_reward(pool, address)?;
        let next = current
            .checked_add(amount)
            .ok_or(AgentRewardError::ClaimableRewardsOverflow)?;
        self.write_pool_claimable_reward(pool, address, next)
    }

    fn write_pool_claimable_reward(
        &mut self,
        pool: RewardPool,
        address: Address,
        amount: U256,
    ) -> Result<()> {
        match pool {
            RewardPool::Waa => self.waa_claimable_rewards.write(&address, amount),
            RewardPool::Sra => self.sra_claimable_rewards.write(&address, amount),
            RewardPool::Cca => self.cca_claimable_rewards.write(&address, amount),
        }
    }

    /// Claims `amount` of the pool's balance as a Gem, or all of it when `amount`
    /// is zero. The function issues the Gem, burns the native COEN that backed it,
    /// and clears the converted amount. The Gem load is not that COEN. The load
    /// becomes Promis at mining time. Leaving the backing in place would therefore
    /// let one emission exist twice.
    ///
    /// The balance is the safe form of the reward. The Gem is not: an unsettled
    /// Gem can be Called and forfeited. Sizing the claim is therefore the agent's
    /// own risk control. The part that the agent does not claim keeps accruing.
    ///
    /// The Gem carries the caller's `(issuance, reference)` currencies. Its entry
    /// price is the previous UTC day's COEN VWAP in the reference currency, which
    /// must be an Oracle reference currency.
    ///
    /// Any failure reverts the call and leaves the balance intact. A missing VWAP
    /// waits for the next day, which brings a new VWAP with it.
    pub fn claim_reward(
        &mut self,
        readers: ExecutionReaders<'_, '_, impl ParentBodySource>,
        pool: RewardPool,
        address: Address,
        amount: U256,
        currencies: (u16, u16),
    ) -> Result<U256> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            if pool == RewardPool::Cca {
                // Registration history is sufficient: earned rewards survive deregistration.
                outbe_ccaregistry::api::cca_state(&storage, address)?;
            }
            let balance = self.get_pool_claimable_reward(pool, address)?;
            if balance.is_zero() {
                return Err(AgentRewardError::NoClaimableBalance.into());
            }
            let requested = if amount.is_zero() { balance } else { amount };
            if requested > balance {
                return Err(AgentRewardError::InsufficientClaimableBalance.into());
            }
            // The balance is native COEN. A Gem load is a protocol amount. The claim
            // mints and burns only the part that survives the conversion, so a
            // sub-unit remainder keeps accumulating instead of being lost.
            let gem_load = native_to_protocol_floor(requested);
            if gem_load.is_zero() {
                return Err(AgentRewardError::ClaimBelowProtocolUnit.into());
            }
            let burned = checked_protocol_to_native(gem_load)
                .ok_or(AgentRewardError::NativeClaimOverflow)?;
            let gem_type = match pool {
                RewardPool::Waa => GemTypes::Wallet,
                RewardPool::Sra => GemTypes::Sra,
                RewardPool::Cca => GemTypes::Cca,
            };
            let gem_id = issue_reward_gem(
                &self.storage,
                readers,
                (address, gem_load),
                gem_type,
                currencies,
            )?;
            self.storage
                .decrease_balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS, burned)?;
            self.write_pool_claimable_reward(pool, address, balance - burned)?;

            if pool == RewardPool::Cca {
                self.emit(IAgentReward::RewardsClaimed {
                    cca: address,
                    amount: burned,
                })?;
            }
            Ok(gem_id)
        })
    }

    /// Gets all WAA tribute counts for a day as (address, count) pairs.
    pub fn get_all_waa_counts(&self, day: WorldwideDay) -> Result<Vec<(Address, u64)>> {
        read_tribute_counts(
            day,
            &self.waa_address_count,
            &self.waa_addresses,
            &self.waa_tribute_counts,
        )
    }

    /// Gets all SRA tribute counts for a day as (address, count) pairs.
    pub fn get_all_sra_counts(&self, day: WorldwideDay) -> Result<Vec<(Address, u64)>> {
        read_tribute_counts(
            day,
            &self.sra_address_count,
            &self.sra_addresses,
            &self.sra_tribute_counts,
        )
    }

    /// Clears WAA tribute counts and address list for a day. The distribution
    /// path calls this function after it settles the day's WAA pool.
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

    /// Clears SRA tribute counts and address list for a day. The distribution
    /// path calls this function after it settles the day's SRA pool.
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

fn read_tribute_counts(
    day: WorldwideDay,
    address_count: &Map<'_, WorldwideDay, u32>,
    addresses: &Map<'_, B256, Address>,
    tribute_counts: &Map<'_, B256, u64>,
) -> Result<Vec<(Address, u64)>> {
    let addr_count = address_count.read(&day)?;
    let mut result = Vec::with_capacity(addr_count as usize);
    for i in 0..addr_count {
        let idx_key = AgentRewardContract::address_index_key(day, i);
        let addr = addresses.read(&idx_key)?;
        if addr.is_zero() {
            continue;
        }
        let count_key = AgentRewardContract::tribute_count_key(day, addr);
        let count = tribute_counts.read(&count_key)?;
        if count > 0 {
            result.push((addr, count));
        }
    }
    Ok(result)
}

/// Shared issuance economics and claim-day pricing for WAA, SRA and CCA rewards.
pub(crate) fn issue_reward_gem(
    storage: &StorageHandle<'_>,
    readers: ExecutionReaders<'_, '_, impl ParentBodySource>,
    recipient: (Address, U256),
    gem_type: GemTypes,
    (issuance_currency, reference_currency): (u16, u16),
) -> Result<U256> {
    let ExecutionReaders { scope, parent } = readers;
    let (owner, load) = recipient;
    outbe_oracle::api::check_reference_currency_with_storage(storage.clone(), reference_currency)?;
    let entry_price = resolve_gem_entry_price(storage, reference_currency)?.ok_or(
        AgentRewardError::MissingPreviousDayPrice(reference_currency),
    )?;
    outbe_gemfactory::api::issue_gem(
        storage,
        scope,
        parent,
        outbe_gemfactory::GemIssueParams {
            owner,
            gem_type,
            promis_load: load,
            issuance_currency,
            reference_currency,
            entry_price,
        },
    )
}

fn resolve_gem_entry_price(
    storage: &StorageHandle<'_>,
    reference_currency: u16,
) -> Result<Option<U256>> {
    // Unix seconds must fit u64. Reject malformed timestamps instead of truncating.
    let now: u64 = storage
        .timestamp()?
        .try_into()
        .map_err(|_| AgentRewardError::TimestampOverflow)?;
    let day = previous_date_key(timestamp_to_date_key(now));
    outbe_oracle::api::get_utc_day_vwap_for_iso(storage.clone(), day, reference_currency)
}
