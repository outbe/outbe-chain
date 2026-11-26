use crate::constants::{AGENT_DISCOUNT_PCT, REWARD_BALANCE_IDLE_SECONDS};
use crate::errors::AgentRewardError;
use crate::precompile::IAgentReward;
use crate::schema::{AgentRewardContract, RewardPool};
use alloy_primitives::{Address, B256, U256};
use outbe_core_common::markup::marked_down;
use outbe_primitives::error::Result;
use outbe_primitives::storage::dsl::Map;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::time::WorldwideDay;

impl AgentRewardContract<'_> {
    /// Increments WAA (wallet) offer count for an address on `day`.
    ///
    /// On the first offer for this address+day pair, the function also
    /// appends the address to the per-day WAA address list. Distribution
    /// enumerates that list.
    /// Overflow rejects activity before any write. Multi-recipient offer
    /// atomicity requires the caller's checkpoint.
    pub fn increment_waa_offer(&mut self, day: WorldwideDay, address: Address) -> Result<()> {
        self.increment_offer(RewardPool::Waa, day, address)
    }

    /// Increments SRA offer count for an address on `day`.
    ///
    /// On the first offer for this address+day pair, the function also
    /// appends the address to the per-day SRA address list. Distribution
    /// enumerates that list.
    /// Overflow rejects activity before any write. Multi-recipient offer
    /// atomicity requires the caller's checkpoint.
    pub fn increment_sra_offer(&mut self, day: WorldwideDay, address: Address) -> Result<()> {
        self.increment_offer(RewardPool::Sra, day, address)
    }

    fn increment_offer(
        &mut self,
        pool: RewardPool,
        day: WorldwideDay,
        address: Address,
    ) -> Result<()> {
        if day.value() <= outbe_rewards::api::get_last_sealed_day(&self.storage)? {
            return Err(AgentRewardError::DaySealed(day.value()).into());
        }
        let index = self.day_index(pool)?;
        let key = AgentRewardContract::offer_count_key(day, address);
        let count = index.counts.read(&key)?;
        let next_count = count
            .checked_add(1)
            .ok_or(AgentRewardError::OfferCounterOverflow)?;
        let next_total = index
            .total
            .read(&day)?
            .checked_add(1)
            .ok_or(AgentRewardError::OfferCounterOverflow)?;
        if count == 0 {
            // First offer for this address+day - add to address list
            let addr_count = index.address_count.read(&day)?;
            let next_addr_count = addr_count
                .checked_add(1)
                .ok_or(AgentRewardError::OfferCounterOverflow)?;
            let idx_key = AgentRewardContract::address_index_key(day, addr_count);
            index.addresses.write(&idx_key, address)?;
            index.address_count.write(&day, next_addr_count)?;
            index.member_index.write(&key, next_addr_count)?;
        }
        index.total.write(&day, next_total)?;
        index.counts.write(&key, next_count)
    }

    /// Gets the total claimable reward balance for an address across all pools.
    pub fn agent_reward_total(&self, address: Address) -> Result<U256> {
        [
            RewardPool::Waa,
            RewardPool::Sra,
            RewardPool::Cca,
            RewardPool::Validator,
        ]
        .into_iter()
        .try_fold(U256::ZERO, |total, pool| {
            total
                .checked_add(self.get_agent_reward(pool, address)?)
                .ok_or_else(|| AgentRewardError::AgentRewardTotalOverflow.into())
        })
    }

    /// Gets the claimable reward balance an address holds in one pool.
    pub fn get_agent_reward(&self, pool: RewardPool, address: Address) -> Result<U256> {
        match pool {
            RewardPool::Waa => self.waa_claimable_minor.read(&address),
            RewardPool::Sra => self.sra_claimable_minor.read(&address),
            RewardPool::Cca => self.cca_claimable_minor.read(&address),
            RewardPool::Validator => self.validator_claimable_minor.read(&address),
        }
    }

    /// The claimable reward of a pool summed over its addresses.
    pub fn pool_agent_reward_total(&self, pool: RewardPool) -> Result<U256> {
        self.claimable_total_minor.read(&(pool as u8))
    }

    /// The part of the pool's queued day pools not yet credited.
    pub fn pool_pending_agent_reward_total(&self, pool: RewardPool) -> Result<U256> {
        self.pending_total_minor.read(&(pool as u8))
    }

    /// Returns zero for an empty balance; a nonempty balance must have a deadline.
    pub fn agent_reward_deadline(&self, pool: RewardPool, address: Address) -> Result<u64> {
        if self.get_agent_reward(pool, address)?.is_zero() {
            return Ok(0);
        }
        let deadline = self
            .reward_balance_deadlines
            .read(&Self::reward_balance_key(pool, address))?;
        if deadline == 0 {
            return Err(AgentRewardError::RewardDeadlineMissing.into());
        }
        Ok(deadline)
    }

    fn renewed_reward_deadline(&self) -> Result<u64> {
        self.storage
            .now_secs()?
            .checked_add(REWARD_BALANCE_IDLE_SECONDS)
            .ok_or_else(|| AgentRewardError::RewardDeadlineOverflow.into())
    }

    /// Adds amount to an address's claimable reward in one pool.
    pub fn add_agent_reward(
        &mut self,
        pool: RewardPool,
        address: Address,
        amount: U256,
    ) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let current = self.get_agent_reward(pool, address)?;
            let next = current
                .checked_add(amount)
                .ok_or(AgentRewardError::AgentRewardOverflow)?;
            let deadline = if current.is_zero() {
                self.renewed_reward_deadline()?
            } else {
                self.agent_reward_deadline(pool, address)?
            };
            self.write_agent_reward(pool, address, next)?;
            self.reward_balance_deadlines
                .write(&Self::reward_balance_key(pool, address), deadline)
        })
    }

    /// Anyone can return an expired pool balance, but only the Promis Limit receives it.
    pub fn expire_agent_reward(&mut self, pool: RewardPool, address: Address) -> Result<U256> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let balance = self.get_agent_reward(pool, address)?;
            if balance.is_zero() {
                return Ok(U256::ZERO);
            }
            let deadline = self.agent_reward_deadline(pool, address)?;
            if storage.now_secs()? < deadline {
                return Err(AgentRewardError::AgentRewardNotExpired(deadline).into());
            }
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .add(outbe_promislimit::LimitSource::AgentReward, balance)?;
            self.write_agent_reward(pool, address, U256::ZERO)?;
            self.reward_balance_deadlines
                .write(&Self::reward_balance_key(pool, address), 0)?;
            self.emit(IAgentReward::AgentRewardExpired {
                account: address,
                pool: pool as u8,
                promisLoadMinor: balance,
            })?;
            Ok(balance)
        })
    }

    fn write_agent_reward(
        &mut self,
        pool: RewardPool,
        address: Address,
        amount: U256,
    ) -> Result<()> {
        let previous = self.get_agent_reward(pool, address)?;
        let total = self
            .claimable_total_minor
            .read(&(pool as u8))?
            .checked_sub(previous)
            .and_then(|rest| rest.checked_add(amount))
            .ok_or(AgentRewardError::PoolTotalMismatch)?;
        self.claimable_total_minor.write(&(pool as u8), total)?;
        match pool {
            RewardPool::Waa => self.waa_claimable_minor.write(&address, amount),
            RewardPool::Sra => self.sra_claimable_minor.write(&address, amount),
            RewardPool::Cca => self.cca_claimable_minor.write(&address, amount),
            RewardPool::Validator => self.validator_claimable_minor.write(&address, amount),
        }
    }

    /// Claims `amount` of the pool's balance as a Gem of that Promis load, or all
    /// of it when `amount` is zero. Balances and loads are protocol units.
    ///
    /// The balance is the safe form of the reward. The Gem is not: an unsettled
    /// Gem can be Called and forfeited. Sizing the claim is therefore the agent's
    /// own risk control. Unclaimed value expires after 365 days without a claim
    /// in this pool and can then be returned to the Promis Limit by anyone.
    ///
    /// The Gem carries the caller's `(issuance, reference)` currencies. Its entry
    /// price is the 24-hour COEN VWAP of the claim hour in the reference currency less
    /// [`AGENT_DISCOUNT_PCT`]; its call price marks up the full VWAP. The reference
    /// currency must be an Oracle reference currency.
    ///
    /// Any failure reverts the call and leaves the balance intact.
    pub fn redeem_agent_reward(
        &mut self,

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
            let balance = self.get_agent_reward(pool, address)?;
            if balance.is_zero() {
                return Err(AgentRewardError::NoAgentReward.into());
            }
            let deadline = self.agent_reward_deadline(pool, address)?;
            if storage.now_secs()? >= deadline {
                return Err(AgentRewardError::AgentRewardDeadlinePassed(deadline).into());
            }
            let requested = if amount.is_zero() { balance } else { amount };
            if requested > balance {
                return Err(AgentRewardError::InsufficientAgentReward.into());
            }
            let next_deadline = if requested == balance {
                0
            } else {
                self.renewed_reward_deadline()?
            };
            let gem_id = issue_reward_gem(&self.storage, (address, requested), currencies)?;
            self.write_agent_reward(pool, address, balance - requested)?;
            self.reward_balance_deadlines
                .write(&Self::reward_balance_key(pool, address), next_deadline)?;
            self.emit(IAgentReward::AgentRewardRedeemed {
                account: address,
                pool: pool as u8,
                promisLoadMinor: requested,
                gemId: gem_id,
            })?;
            Ok(gem_id)
        })
    }

    /// Gets all WAA offer counts for a day as (address, count) pairs.
    pub fn get_all_waa_counts(&self, day: WorldwideDay) -> Result<Vec<(Address, u64)>> {
        read_offer_counts(
            day,
            &self.waa_address_count,
            &self.waa_addresses,
            &self.waa_offer_counts,
        )
    }

    /// Gets all SRA offer counts for a day as (address, count) pairs.
    pub fn get_all_sra_counts(&self, day: WorldwideDay) -> Result<Vec<(Address, u64)>> {
        read_offer_counts(
            day,
            &self.sra_address_count,
            &self.sra_addresses,
            &self.sra_offer_counts,
        )
    }
}

fn read_offer_counts(
    day: WorldwideDay,
    address_count: &Map<'_, WorldwideDay, u32>,
    addresses: &Map<'_, B256, Address>,
    offer_counts: &Map<'_, B256, u64>,
) -> Result<Vec<(Address, u64)>> {
    let addr_count = address_count.read(&day)?;
    let mut result = Vec::with_capacity(addr_count as usize);
    for i in 0..addr_count {
        let idx_key = AgentRewardContract::address_index_key(day, i);
        let addr = addresses.read(&idx_key)?;
        if addr.is_zero() {
            continue;
        }
        let count_key = AgentRewardContract::offer_count_key(day, addr);
        let count = offer_counts.read(&count_key)?;
        if count > 0 {
            result.push((addr, count));
        }
    }
    Ok(result)
}

/// Shared issuance economics and claim-hour pricing for every reward pool.
pub(crate) fn issue_reward_gem(
    storage: &StorageHandle<'_>,

    recipient: (Address, U256),
    (issuance_currency, reference_currency): (u16, u16),
) -> Result<U256> {
    let (owner, load) = recipient;
    outbe_oracle::api::check_reference_currency(reference_currency)?;
    let snapshot = outbe_oracle::api::current_vwap_24h_snapshot(storage.clone())?;
    let vwap = outbe_oracle::api::vwap_24h(storage.clone(), reference_currency, snapshot)?
        .ok_or(AgentRewardError::RedeemPriceUnavailable(reference_currency))?;
    let entry_price =
        marked_down(vwap, AGENT_DISCOUNT_PCT).ok_or(AgentRewardError::EntryPriceOverflow)?;
    outbe_gemfactory::api::issue_gem(
        storage,
        outbe_gemfactory::GemIssueParams {
            owner,
            promis_load_minor: load,
            issuance_currency,
            reference_currency,
            entry_price_minor: entry_price,
            call_anchor_price_minor: vwap,
        },
    )
}
