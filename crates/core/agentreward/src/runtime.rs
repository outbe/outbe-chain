use crate::constants::{
    AGENT_DISCOUNT_PCT, MAX_REWARD_DAYS_PER_CALL, RATE_SCALE, REWARD_BALANCE_IDLE_SECONDS,
};
use crate::errors::AgentRewardError;
use crate::precompile::IAgentReward;
use crate::schema::{AgentRewardContract, PoolDay, RewardPool};
use alloy_primitives::{Address, U256, U512};
use outbe_core_common::markup::marked_down;
use outbe_emission::schema::{emission_day_state, metadosis_state};
use outbe_primitives::error::{PassFailure, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::{LimitSource, PromisLimitContract};

/// `floor(a * b / c)` with a 512-bit product. `c` is not zero.
fn mul_div(a: U256, b: U256, c: U256) -> Result<U256> {
    let value = (U512::from(a) * U512::from(b))
        .checked_div(U512::from(c))
        .ok_or(AgentRewardError::RateOverflow)?;
    if value > U512::from(U256::MAX) {
        return Err(AgentRewardError::RateOverflow.into());
    }
    Ok(value.wrapping_to::<U256>())
}

fn scale() -> U256 {
    U256::from(RATE_SCALE)
}

/// The amount of `weight` at `rate`.
fn amount_of(rate: U256, weight: U256) -> Result<U256> {
    mul_div(rate, weight, scale())
}

impl AgentRewardContract<'_> {
    /// Counts one WAA offer of `address` for the Nods of emission day `day`.
    /// Multi-recipient offer atomicity requires the caller's checkpoint.
    pub fn increment_waa_offer(&mut self, day: u32, address: Address) -> Result<()> {
        self.increment_offer(RewardPool::Waa, day, address)
    }

    /// Counts one SRA offer of `address` for the Nods of emission day `day`.
    /// Multi-recipient offer atomicity requires the caller's checkpoint.
    pub fn increment_sra_offer(&mut self, day: u32, address: Address) -> Result<()> {
        self.increment_offer(RewardPool::Sra, day, address)
    }

    fn increment_offer(&mut self, pool: RewardPool, day: u32, address: Address) -> Result<()> {
        if self
            .pool_days
            .exists(AgentRewardContract::pool_day_key(pool, day))?
        {
            return Err(AgentRewardError::AgentRewardRateAlreadySet {
                pool: pool as u8,
                day,
            }
            .into());
        }
        let index = self.offer_index(pool)?;
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
            let next_addresses = index
                .address_count
                .read(&day)?
                .checked_add(1)
                .ok_or(AgentRewardError::OfferCounterOverflow)?;
            index.address_count.write(&day, next_addresses)?;
        }
        index.total.write(&day, next_total)?;
        index.counts.write(&key, next_count)?;
        self.emit(IAgentReward::AgentRewardWeightAdded {
            account: address,
            pool: pool as u8,
            emissionDay: day,
            weight: U256::from(next_count),
        })
    }

    /// The weight of `address` in `pool` of emission day `day`.
    pub fn weight(&self, pool: RewardPool, day: u32, address: Address) -> Result<U256> {
        match pool {
            RewardPool::Validator => {
                outbe_rewards::api::validator_weight(&self.storage, day, address)
            }
            RewardPool::Cca => outbe_ccaregistry::api::reward_weight(&self.storage, address, day),
            RewardPool::Waa | RewardPool::Sra => {
                let index = self.offer_index(pool)?;
                Ok(U256::from(index.counts.read(
                    &AgentRewardContract::offer_count_key(day, address),
                )?))
            }
        }
    }

    /// Reads and deletes the weight of `address`; its owner module deletes it.
    fn take_weight(&self, pool: RewardPool, day: u32, address: Address) -> Result<U256> {
        match pool {
            RewardPool::Validator => {
                outbe_rewards::api::take_validator_weight(&self.storage, day, address)
            }
            RewardPool::Cca => {
                outbe_ccaregistry::api::take_reward_weight(&self.storage, address, day)
            }
            RewardPool::Waa | RewardPool::Sra => {
                let index = self.offer_index(pool)?;
                let key = AgentRewardContract::offer_count_key(day, address);
                let count = index.counts.read(&key)?;
                if count != 0 {
                    index.counts.write(&key, 0)?;
                }
                Ok(U256::from(count))
            }
        }
    }

    /// The number of the receivers of `pool` in `day` and the sum of their weights.
    fn day_inputs(&self, pool: RewardPool, day: u32) -> Result<(u32, U256)> {
        match pool {
            RewardPool::Validator => outbe_rewards::api::validator_day_weights(&self.storage, day),
            RewardPool::Cca => outbe_ccaregistry::api::reward_day_inputs(&self.storage, day),
            RewardPool::Waa | RewardPool::Sra => {
                let index = self.offer_index(pool)?;
                Ok((
                    index.address_count.read(&day)?,
                    U256::from(index.total.read(&day)?),
                ))
            }
        }
    }

    /// Deletes the count and the weight sum of `pool` in `day`, with its last weight.
    fn clear_day(&self, pool: RewardPool, day: u32) -> Result<()> {
        match pool {
            RewardPool::Validator => outbe_rewards::api::clear_validator_day(&self.storage, day),
            RewardPool::Cca => outbe_ccaregistry::api::clear_reward_day(&self.storage, day),
            RewardPool::Waa | RewardPool::Sra => {
                let index = self.offer_index(pool)?;
                index.address_count.write(&day, 0)?;
                index.total.write(&day, 0)
            }
        }
    }

    fn limit_returned(&mut self, pool: RewardPool, day: u32, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        PromisLimitContract::new(self.storage.clone()).add(LimitSource::RewardLimit, amount)?;
        self.emit(IAgentReward::AgentRewardLimitReturned {
            emissionDay: day,
            pool: pool as u8,
            amountMinor: amount,
        })
    }

    /// Checks that the rate of `pool` and `day` is due and returns the pool limit. The Validator
    /// and CCA rates are set before the record of the day starts, the WAA and SRA rates after it
    /// started and before its Metadosis ends.
    fn due_pool_limit(&self, pool: RewardPool, day: u32) -> Result<U256> {
        let not_due = || AgentRewardError::RateNotDue {
            pool: pool as u8,
            day,
        };
        let record = outbe_emission::api::get_emission_day(self.storage.clone(), day)?
            .ok_or_else(not_due)?;
        let due = match pool {
            RewardPool::Validator | RewardPool::Cca => record.state == emission_day_state::PENDING,
            RewardPool::Waa | RewardPool::Sra => {
                record.state == emission_day_state::PROCESSING
                    && record.metadosis_state == metadosis_state::PENDING
            }
        };
        if !due
            || self
                .pool_days
                .exists(AgentRewardContract::pool_day_key(pool, day))?
        {
            return Err(not_due().into());
        }
        Ok(U256::from(match pool {
            RewardPool::Validator => record.validators_limit,
            RewardPool::Cca => record.cca_limit,
            RewardPool::Waa => record.waa_limit,
            RewardPool::Sra => record.sra_limit,
        }))
    }

    fn reward_deadline(&self) -> Result<u64> {
        self.storage
            .now_secs()?
            .checked_add(REWARD_BALANCE_IDLE_SECONDS)
            .ok_or_else(|| AgentRewardError::RewardDeadlineOverflow.into())
    }

    /// Stores the rate record of a pool and day that has receivers.
    fn store_pool_day(
        &mut self,
        pool: RewardPool,
        day: u32,
        (rate, allocated): (U256, U256),
        receivers: u32,
    ) -> Result<u64> {
        let deadline = self.reward_deadline()?;
        if receivers != 0 {
            self.pool_days.create(&PoolDay {
                key: AgentRewardContract::pool_day_key(pool, day),
                rate,
                unclaimed_minor: allocated,
                deadline,
                receivers,
            })?;
        }
        Ok(deadline)
    }

    fn set_rate(&mut self, pool: RewardPool, day: u32, limit: U256) -> Result<()> {
        let (receivers, total_weight) = self.day_inputs(pool, day)?;
        let (rate, allocated) = if total_weight.is_zero() {
            (U256::ZERO, U256::ZERO)
        } else {
            let rate = mul_div(limit, scale(), total_weight)?;
            (rate, amount_of(rate, total_weight)?.min(limit))
        };
        self.limit_returned(pool, day, limit - allocated)?;
        outbe_emission::api::record_reward_pool(
            self.storage.clone(),
            day,
            pool.emission_pool(),
            allocated,
        )?;
        let receivers = if total_weight.is_zero() { 0 } else { receivers };
        let deadline = self.store_pool_day(pool, day, (rate, allocated), receivers)?;
        self.emit(IAgentReward::AgentRewardRateSet {
            emissionDay: day,
            pool: pool as u8,
            rate,
            allocatedMinor: allocated,
            deadline,
        })
    }

    /// The whole pool limit goes to the Promis Limit. The weights stay until they expire; when
    /// they cannot be read, the pool keeps no rate record.
    fn fail_rate(&mut self, pool: RewardPool, day: u32, limit: U256) -> Result<()> {
        PromisLimitContract::new(self.storage.clone()).add(LimitSource::RewardLimit, limit)?;
        outbe_emission::api::record_reward_pool(
            self.storage.clone(),
            day,
            pool.emission_pool(),
            U256::ZERO,
        )?;
        let receivers = match self.day_inputs(pool, day) {
            Ok((receivers, _)) => receivers,
            Err(err) if err.record_pass_failure() == PassFailure::Skip => 0,
            Err(err) => return Err(err),
        };
        self.store_pool_day(pool, day, (U256::ZERO, U256::ZERO), receivers)?;
        self.emit(IAgentReward::AgentRewardRateFailed {
            emissionDay: day,
            pool: pool as u8,
            amountMinor: limit,
        })
    }

    /// Sets the rate of `pool` for emission day `day`, writes the allocated amount into the
    /// record of the day and sends the rest of the pool limit to the Promis Limit. A step that
    /// reverts sends the whole pool limit there instead.
    pub fn set_agent_reward_rate(&mut self, pool: RewardPool, day: u32) -> Result<()> {
        let limit = self.due_pool_limit(pool, day)?;
        let storage = self.storage.clone();
        match storage.with_checkpoint(|| self.set_rate(pool, day, limit)) {
            Err(err) if err.record_pass_failure() == PassFailure::Skip => {
                storage.with_checkpoint(|| self.fail_rate(pool, day, limit))
            }
            result => result,
        }
    }

    /// Takes the weight of `address` in one day and returns its amount. The last weight of the
    /// day sends the rounding rest to the Promis Limit and deletes the values of the day.
    fn settle_day(
        &mut self,
        pool: RewardPool,
        day: u32,
        address: Address,
        expiring: bool,
    ) -> Result<U256> {
        let key = AgentRewardContract::pool_day_key(pool, day);
        let mut pool_day =
            self.pool_days
                .get(key)?
                .ok_or(AgentRewardError::AgentRewardRateMissing {
                    pool: pool as u8,
                    day,
                })?;
        let now = self.storage.now_secs()?;
        if expiring && now < pool_day.deadline {
            return Err(AgentRewardError::AgentRewardNotExpired(pool_day.deadline).into());
        }
        if !expiring && now >= pool_day.deadline {
            return Err(AgentRewardError::AgentRewardDeadlinePassed(pool_day.deadline).into());
        }
        let weight = self.take_weight(pool, day, address)?;
        if weight.is_zero() {
            return Err(AgentRewardError::NoAgentRewardWeight(day).into());
        }
        let amount = amount_of(pool_day.rate, weight)?;
        let underflow = AgentRewardError::UnclaimedUnderflow {
            pool: pool as u8,
            day,
        };
        pool_day.unclaimed_minor = pool_day
            .unclaimed_minor
            .checked_sub(amount)
            .ok_or(underflow)?;
        pool_day.receivers = pool_day.receivers.checked_sub(1).ok_or(underflow)?;
        if pool_day.receivers == 0 {
            self.limit_returned(pool, day, pool_day.unclaimed_minor)?;
            self.pool_days.delete(key)?;
            self.clear_day(pool, day)?;
        } else {
            self.pool_days.update(&pool_day)?;
        }
        Ok(amount)
    }

    /// Claims the reward of `address` in `pool` for each of `days` as one Gem.
    ///
    /// The Gem carries the caller's `(issuance, reference)` currencies. Its entry price is the
    /// 24-hour COEN VWAP of the claim hour in the reference currency less
    /// [`AGENT_DISCOUNT_PCT`]; its call price marks up the full VWAP. Any failure reverts the call.
    pub fn redeem_agent_reward(
        &mut self,
        pool: RewardPool,
        address: Address,
        days: &[u32],
        currencies: (u16, u16),
    ) -> Result<U256> {
        require_days(days)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            if pool == RewardPool::Cca {
                // Registration history is sufficient: earned rewards survive deregistration.
                outbe_ccaregistry::api::cca_state(&storage, address)?;
            }
            let mut total = U256::ZERO;
            for &day in days {
                let amount = self.settle_day(pool, day, address, false)?;
                total = total
                    .checked_add(amount)
                    .ok_or(AgentRewardError::RateOverflow)?;
            }
            if total.is_zero() {
                return Err(AgentRewardError::NoAgentReward.into());
            }
            let gem_id = issue_reward_gem(&self.storage, (address, total), currencies)?;
            self.emit(IAgentReward::AgentRewardRedeemed {
                account: address,
                pool: pool as u8,
                promisLoadMinor: total,
                gemId: gem_id,
            })?;
            Ok(gem_id)
        })
    }

    /// Sends the reward of `address` in `pool` for each of `days`, past its deadline, to the
    /// Promis Limit. Any account may call it.
    pub fn expire_agent_reward(
        &mut self,
        pool: RewardPool,
        address: Address,
        days: &[u32],
    ) -> Result<U256> {
        require_days(days)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let mut total = U256::ZERO;
            for &day in days {
                let amount = self.settle_day(pool, day, address, true)?;
                if !amount.is_zero() {
                    PromisLimitContract::new(storage.clone())
                        .add(LimitSource::AgentReward, amount)?;
                }
                self.emit(IAgentReward::AgentRewardExpired {
                    account: address,
                    pool: pool as u8,
                    emissionDay: day,
                    promisLoadMinor: amount,
                })?;
                total = total
                    .checked_add(amount)
                    .ok_or(AgentRewardError::RateOverflow)?;
            }
            Ok(total)
        })
    }

    /// The rate, the deadline and the unclaimed amount of `pool` and `day`; zero when none.
    pub fn agent_reward_rate(&self, pool: RewardPool, day: u32) -> Result<(U256, u64, U256)> {
        Ok(self
            .pool_days
            .get(AgentRewardContract::pool_day_key(pool, day))?
            .map_or((U256::ZERO, 0, U256::ZERO), |pool_day| {
                (pool_day.rate, pool_day.deadline, pool_day.unclaimed_minor)
            }))
    }

    /// The weight of `address` in `pool` and `day` and its amount at the rate of the day.
    pub fn agent_reward(
        &self,
        pool: RewardPool,
        day: u32,
        address: Address,
    ) -> Result<(U256, U256)> {
        let weight = self.weight(pool, day, address)?;
        let (rate, _, _) = self.agent_reward_rate(pool, day)?;
        Ok((weight, amount_of(rate, weight)?))
    }
}

fn require_days(days: &[u32]) -> Result<()> {
    if days.is_empty() || days.len() > MAX_REWARD_DAYS_PER_CALL {
        return Err(AgentRewardError::InvalidRewardDays(days.len()).into());
    }
    Ok(())
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
