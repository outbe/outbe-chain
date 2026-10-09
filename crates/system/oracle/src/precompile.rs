//! ABI surface and EVM dispatch for the Oracle precompile.
//!
//! [`dispatch`] routes each call to one handler. The handlers below are grouped
//! by domain: exchange rates, the pair registry, votes, price series, and the
//! S-curve.

use crate::errors::OracleError;
use crate::schema::OracleContract;
use crate::window::{active_vwap_policy, get_vwap_snapshot_id, VwapSnapshotId};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolEvent, SolInterface};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::dispatch::{dispatch_call, metadata, mutate_void, reject_value, view};
use outbe_primitives::error::Result;
use outbe_primitives::math::reference_price::pair_scales;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IOracle.sol"
);

/// Splits a pair list into the parallel `(bases, quotes)` columns the ABI
/// returns. It preserves each pair's registered orientation, so a caller can
/// quote the result straight back into the pair-scoped reads.
fn split_pairs(pairs: &[AddressPair]) -> (Vec<Address>, Vec<Address>) {
    pairs.iter().map(|p| (p.address1(), p.address2())).unzip()
}

/// Runs one non-payable state change for the caller. Rejects attached value
/// first. After `apply` succeeds, emits the event that `apply` returns from the
/// Oracle address.
fn mutate_and_emit<C: SolCall, E: SolEvent>(
    oracle: &mut OracleContract,
    call: C,
    caller: Address,
    value: U256,
    apply: impl FnOnce(&mut OracleContract, Address, C) -> Result<E>,
) -> Result<Bytes>
where
    C::Return: From<()>,
{
    reject_value(&value)?;
    let storage = oracle.storage.clone();
    mutate_void(&storage, call, caller, |sender, call| {
        let event = apply(oracle, sender, call)?;
        let _ = oracle
            .storage
            .emit_event(ORACLE_ADDRESS, event.encode_log_data());
        Ok(())
    })
}

/// Dispatches an ABI-encoded call to the Oracle precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    dispatch_call(data, IOracle::IOracleCalls::abi_decode, |call| {
        let mut oracle = OracleContract::new(storage);
        let oracle = &mut oracle;
        use IOracle::IOracleCalls::*;
        match call {
            getExchangeRate(c) => exchange_rate(oracle, c),
            getExchangeRateData(c) => exchange_rate_data(oracle, c),
            getCoenExchangeRateFor(c) => coen_exchange_rate_for(oracle, c),
            currencyCrossRate(c) => currency_cross_rate(oracle, c),
            getVwap(c) => vwap(oracle, c),
            getParams(_) => params(oracle),
            getVotePenaltyCounter(c) => vote_penalty_counter(oracle, c),
            getFeederDelegation(c) => feeder_delegation(oracle, c),
            isVoteTarget(c) => is_vote_target(oracle, c),
            getPairCount(_) => pair_count(oracle),
            getPairByIndex(c) => pair_by_index(oracle, c),
            getVoteTargets(_) => vote_targets(oracle),
            getAggregateVote(c) => aggregate_vote(oracle, c),
            getSlashWindowProgress(c) => slash_window_progress(oracle, c),
            getVwapForTimeRange(c) => vwap_for_time_range(oracle, c),
            getUtcDayVwap(c) => utc_day_vwap(oracle, c),
            getScurveValue(c) => scurve_value(oracle, c),
            setExchangeRate(c) => set_exchange_rate(oracle, c, caller, value),
            delegateFeederConsent(c) => delegate_feeder_consent(oracle, c, caller, value),
            deactivateVoteTarget(c) => deactivate_vote_target(oracle, c, caller, value),
            activateVoteTarget(c) => activate_vote_target(oracle, c, caller, value),
            getPriceSnapshotHistory(c) => price_snapshot_history(oracle, c),
            getAllPriceSnapshotHistory(c) => all_price_snapshot_history(oracle, c),
            getTwap(c) => twap(oracle, c),
            getTwaps(c) => twaps(oracle, c),
            getDayVwap(c) => day_vwap(oracle, c),
            getVwapPolicy(_) => vwap_policy(),
            getVwapSnapshotId(_) => vwap_snapshot_id(oracle),
            getFinalizedWindowVwap(c) => finalized_window_vwap(oracle, c),
            getWorldwideDayVwap(c) => worldwide_day_vwap(oracle, c),
            getWorldwideDayVwapSnapshot(c) => worldwide_day_vwap_snapshot(oracle, c),
            getScurveEntries(c) => scurve_entries(oracle, c),
            getScurveValues(c) => scurve_values(oracle, c),
            getAllScurveData(_) => all_scurve_data(oracle),
            getAllScurveDataForPair(c) => all_scurve_data_for_pair(oracle, c),
            getReferenceCurrencies(_) => reference_currencies(oracle),
            getPolicyRateCurrencies(_) => policy_rate_currencies(oracle),
            getPolicyRate(c) => policy_rate(oracle, c),
            getNominalPrice(c) => nominal_price(oracle, c),
            getNominalPriceComponents(c) => nominal_price_components(oracle, c),
            submitVote(c) => submit_vote(oracle, c, caller, value),
        }
    })
}

// ---------------------------------------------------------------------------
// Exchange rates
// ---------------------------------------------------------------------------

fn exchange_rate(oracle: &OracleContract, c: IOracle::getExchangeRateCall) -> Result<Bytes> {
    view(c, |c| oracle.get_exchange_rate(c.base, c.quote))
}

fn exchange_rate_data(
    oracle: &OracleContract,
    c: IOracle::getExchangeRateDataCall,
) -> Result<Bytes> {
    view(c, |c| {
        let (rate, block, ts) = oracle.get_exchange_rate_data(c.base, c.quote)?;
        Ok((rate, block, ts).into())
    })
}

fn coen_exchange_rate_for(
    oracle: &OracleContract,
    c: IOracle::getCoenExchangeRateForCall,
) -> Result<Bytes> {
    view(c, |c| {
        let quote = crate::api::currency_address(c.isoCode);
        oracle.get_exchange_rate(crate::api::COEN_ASSET, quote)
    })
}

fn currency_cross_rate(
    oracle: &OracleContract,
    c: IOracle::currencyCrossRateCall,
) -> Result<Bytes> {
    view(c, |c| {
        crate::api::currency_cross_rate(oracle.storage.clone(), c.fromIso, c.toIso, c.amount)
    })
}

fn reference_currencies(oracle: &OracleContract) -> Result<Bytes> {
    metadata::<IOracle::getReferenceCurrenciesCall>(|| oracle.reference_currencies.read_all())
}

fn policy_rate_currencies(oracle: &OracleContract) -> Result<Bytes> {
    metadata::<IOracle::getPolicyRateCurrenciesCall>(|| oracle.policy_rate_currencies.read_all())
}

fn policy_rate(oracle: &OracleContract, c: IOracle::getPolicyRateCall) -> Result<Bytes> {
    view(c, |c| oracle.get_policy_rate(c.isoCode))
}

fn set_exchange_rate(
    oracle: &mut OracleContract,
    c: IOracle::setExchangeRateCall,
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    mutate_and_emit(oracle, c, caller, value, |oracle, sender, c| {
        // Block number and timestamp are available on the storage handle.
        // Bootstrap writes still store 0 for both.
        // Tally overwrites them with real values.
        // `fresh_rate_at_index` treats a zero timestamp as stale.
        oracle.set_exchange_rate(
            sender,
            AddressPair::from_addresses(c.base, c.quote),
            c.rate,
            0,
            0,
        )?;
        Ok(IOracle::ExchangeRateSet {
            base: c.base,
            quote: c.quote,
            rate: c.rate,
        })
    })
}

// ---------------------------------------------------------------------------
// Configuration and pair registry
// ---------------------------------------------------------------------------

fn params(oracle: &OracleContract) -> Result<Bytes> {
    metadata::<IOracle::getParamsCall>(|| {
        let vote_period = oracle.config_vote_period.read()?;
        let reward_band = oracle.config_reward_band.read()?;
        let slash_window = oracle.config_slash_window.read()?;
        let min_valid = oracle.config_min_valid_per_window.read()?;
        let slash_fraction = oracle.config_slash_fraction.read()?;
        let lookback = oracle.config_lookback_duration.read()?;
        let enabled = oracle.config_enabled.read()?;
        Ok((
            vote_period,
            reward_band,
            slash_window,
            min_valid,
            slash_fraction,
            lookback,
            enabled,
        )
            .into())
    })
}

fn is_vote_target(oracle: &OracleContract, c: IOracle::isVoteTargetCall) -> Result<Bytes> {
    view(c, |c| oracle.is_vote_target(c.base, c.quote))
}

fn pair_count(oracle: &OracleContract) -> Result<Bytes> {
    metadata::<IOracle::getPairCountCall>(|| oracle.pair_count.read())
}

fn pair_by_index(oracle: &OracleContract, c: IOracle::getPairByIndexCall) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_at(c.index)?;
        let (base_scale, quote_scale) = pair_scales(pair);
        Ok((pair.address1(), pair.address2(), base_scale, quote_scale).into())
    })
}

fn vote_targets(oracle: &OracleContract) -> Result<Bytes> {
    metadata::<IOracle::getVoteTargetsCall>(|| {
        let (bases, quotes) = oracle.get_vote_targets()?;
        Ok((bases, quotes).into())
    })
}

fn deactivate_vote_target(
    oracle: &mut OracleContract,
    c: IOracle::deactivateVoteTargetCall,
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    mutate_and_emit(oracle, c, caller, value, |oracle, sender, c| {
        oracle.deactivate_vote_target(sender, c.base, c.quote)?;
        Ok(IOracle::VoteTargetDeactivated {
            base: c.base,
            quote: c.quote,
        })
    })
}

fn activate_vote_target(
    oracle: &mut OracleContract,
    c: IOracle::activateVoteTargetCall,
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    mutate_and_emit(oracle, c, caller, value, |oracle, sender, c| {
        oracle.activate_vote_target(sender, c.base, c.quote)?;
        Ok(IOracle::VoteTargetActivated {
            base: c.base,
            quote: c.quote,
        })
    })
}

// ---------------------------------------------------------------------------
// Votes, feeders and penalty counters
// ---------------------------------------------------------------------------

fn vote_penalty_counter(
    oracle: &OracleContract,
    c: IOracle::getVotePenaltyCounterCall,
) -> Result<Bytes> {
    view(c, |c| {
        let success = oracle.penalty_success_count.read(&c.validator)?;
        let abstain = oracle.penalty_abstain_count.read(&c.validator)?;
        let miss = oracle.penalty_miss_count.read(&c.validator)?;
        Ok((success, abstain, miss).into())
    })
}

fn feeder_delegation(
    oracle: &OracleContract,
    c: IOracle::getFeederDelegationCall,
) -> Result<Bytes> {
    view(c, |c| oracle.get_feeder(&c.validator))
}

fn aggregate_vote(oracle: &OracleContract, c: IOracle::getAggregateVoteCall) -> Result<Bytes> {
    view(c, |c| {
        let (exists, bases, quotes, rates, volumes) = oracle.get_aggregate_vote(&c.validator)?;
        Ok((exists, bases, quotes, rates, volumes).into())
    })
}

fn slash_window_progress(
    oracle: &OracleContract,
    c: IOracle::getSlashWindowProgressCall,
) -> Result<Bytes> {
    view(c, |c| {
        let (success, abstain, miss, slash_window) =
            oracle.get_slash_window_progress(&c.validator)?;
        Ok((success, abstain, miss, slash_window).into())
    })
}

fn delegate_feeder_consent(
    oracle: &mut OracleContract,
    c: IOracle::delegateFeederConsentCall,
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    mutate_and_emit(oracle, c, caller, value, |oracle, sender, c| {
        oracle.delegate_feeder(sender, c.feeder)?;
        Ok(IOracle::FeederDelegated {
            validator: sender,
            feeder: c.feeder,
        })
    })
}

fn submit_vote(
    oracle: &mut OracleContract,
    c: IOracle::submitVoteCall,
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    mutate_and_emit(oracle, c, caller, value, |oracle, sender, c| {
        let tuple_count = c.tuples.len() as u32;
        let tuples: Vec<_> = c
            .tuples
            .iter()
            .map(|t| (t.base, t.quote, t.exchangeRate, t.volume))
            .collect();
        let validator = oracle.submit_vote(sender, &tuples)?;
        // Emit event after successful vote
        Ok(IOracle::VoteSubmitted {
            validator,
            tupleCount: tuple_count,
        })
    })
}

// ---------------------------------------------------------------------------
// Price series
// ---------------------------------------------------------------------------

fn vwap(oracle: &OracleContract, c: IOracle::getVwapCall) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_from(c.base, c.quote)?;
        let now = oracle.storage.timestamp()?.to::<u64>();
        oracle.calculate_vwap_lookback(pair, now, c.lookbackSeconds)
    })
}

fn vwap_for_time_range(
    oracle: &OracleContract,
    c: IOracle::getVwapForTimeRangeCall,
) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_from(c.base, c.quote)?;
        oracle.calculate_vwap(pair, c.startTime, c.endTime)
    })
}

fn utc_day_vwap(oracle: &OracleContract, c: IOracle::getUtcDayVwapCall) -> Result<Bytes> {
    view(c, |c| {
        let index = oracle.require_pair_index(AddressPair::from_addresses(c.base, c.quote))?;
        match oracle.get_utc_day_vwap_for_pair(c.utcDay, index)? {
            Some(vwap) => Ok(vwap),
            None => Err(OracleError::NoFinalizedUtcDayVwap.into()),
        }
    })
}

fn price_snapshot_history(
    oracle: &OracleContract,
    c: IOracle::getPriceSnapshotHistoryCall,
) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_from(c.base, c.quote)?;
        let (timestamps, rates, volumes) = oracle.get_price_snapshot_history(pair, c.count)?;
        Ok(IOracle::getPriceSnapshotHistoryReturn {
            timestamps,
            rates,
            volumes,
        })
    })
}

fn all_price_snapshot_history(
    oracle: &OracleContract,
    c: IOracle::getAllPriceSnapshotHistoryCall,
) -> Result<Bytes> {
    view(c, |c| {
        let (snapshot_ids, timestamps, bases, quotes, rates, volumes) =
            oracle.get_all_price_snapshot_history(c.count)?;
        Ok(IOracle::getAllPriceSnapshotHistoryReturn {
            snapshotIds: snapshot_ids,
            timestamps,
            bases,
            quotes,
            rates,
            volumes,
        })
    })
}

fn twap(oracle: &OracleContract, c: IOracle::getTwapCall) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_from(c.base, c.quote)?;
        let now = oracle.storage.timestamp()?.to::<u64>();
        oracle.calculate_twap(pair, now, c.lookbackSeconds)
    })
}

fn twaps(oracle: &OracleContract, c: IOracle::getTwapsCall) -> Result<Bytes> {
    view(c, |c| {
        let now = oracle.storage.timestamp()?.to::<u64>();
        let (pairs, twaps, lookbacks) = oracle.calculate_twaps(now, c.lookback)?;
        let (bases, quotes) = split_pairs(&pairs);
        Ok(IOracle::getTwapsReturn {
            bases,
            quotes,
            twaps,
            lookbackSeconds: lookbacks,
        })
    })
}

fn day_vwap(oracle: &OracleContract, c: IOracle::getDayVwapCall) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_from(c.base, c.quote)?;
        let now = oracle.storage.timestamp()?.to::<u64>();
        oracle.calculate_vwap_lookback(pair, now, 86400)
    })
}

fn vwap_policy() -> Result<Bytes> {
    metadata::<IOracle::getVwapPolicyCall>(|| {
        let policy = active_vwap_policy();
        Ok(IOracle::getVwapPolicyReturn {
            policyVersion: policy.policy_version,
            vwapLookbackSeconds: policy.vwap_lookback_seconds,
            vwapUpdateIntervalSeconds: policy.vwap_update_interval_seconds,
        })
    })
}

fn vwap_snapshot_id(oracle: &OracleContract) -> Result<Bytes> {
    metadata::<IOracle::getVwapSnapshotIdCall>(|| {
        let now = oracle.storage.timestamp()?.to::<u64>();
        Ok(get_vwap_snapshot_id(now, &active_vwap_policy())?.to_u256())
    })
}

fn finalized_window_vwap(
    oracle: &OracleContract,
    c: IOracle::getFinalizedWindowVwapCall,
) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair(AddressPair::new_coen_to(c.currency))?;
        let snapshot = VwapSnapshotId::from_u256(c.snapshotId)?;
        oracle
            .finalized_window_vwap(pair, snapshot)?
            .ok_or_else(|| OracleError::NoVwapData.into())
    })
}

fn worldwide_day_vwap(
    oracle: &OracleContract,
    c: IOracle::getWorldwideDayVwapCall,
) -> Result<Bytes> {
    view(c, |c| {
        let (pairs, vwaps, lookbacks) = oracle.calculate_vwaps(c.startTime, c.endTime)?;
        let (bases, quotes) = split_pairs(&pairs);
        Ok(IOracle::getWorldwideDayVwapReturn {
            bases,
            quotes,
            vwaps,
            lookbackSeconds: lookbacks,
        })
    })
}

fn worldwide_day_vwap_snapshot(
    oracle: &OracleContract,
    c: IOracle::getWorldwideDayVwapSnapshotCall,
) -> Result<Bytes> {
    view(c, |c| {
        let (start_time, end_time, bases, quotes, vwaps, lookbacks) =
            oracle.get_worldwide_day_vwap_snapshot(c.worldwideDay.into())?;
        Ok(IOracle::getWorldwideDayVwapSnapshotReturn {
            startTime: start_time,
            endTime: end_time,
            bases,
            quotes,
            vwaps,
            lookbackSeconds: lookbacks,
        })
    })
}

// ---------------------------------------------------------------------------
// S-curve and nominal price
// ---------------------------------------------------------------------------

fn scurve_value(oracle: &OracleContract, c: IOracle::getScurveValueCall) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_from(c.base, c.quote)?;
        crate::scurve::get_max_active_scurve_value(oracle, pair, c.timestamp)
    })
}

fn scurve_entries(oracle: &OracleContract, c: IOracle::getScurveEntriesCall) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_from(c.base, c.quote)?;
        let now = oracle.storage.timestamp()?.to::<u64>();
        let (peak_days, peak_prices, current_values) =
            crate::scurve::get_scurve_entries(oracle, pair, now)?;
        Ok(IOracle::getScurveEntriesReturn {
            peakDays: peak_days,
            peakPrices: peak_prices,
            currentValues: current_values,
        })
    })
}

fn scurve_values(oracle: &OracleContract, c: IOracle::getScurveValuesCall) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_from(c.base, c.quote)?;
        let target_day = crate::scurve::truncate_to_day(c.timestamp);
        let (peak_days, peak_prices, values) =
            crate::scurve::get_scurve_entries(oracle, pair, c.timestamp)?;
        Ok(IOracle::getScurveValuesReturn {
            targetDay: target_day,
            peakDays: peak_days,
            peakPrices: peak_prices,
            values,
        })
    })
}

fn all_scurve_data(oracle: &OracleContract) -> Result<Bytes> {
    metadata::<IOracle::getAllScurveDataCall>(|| {
        let (bases, quotes, peak_days, peak_prices) = crate::scurve::get_all_scurve_data(oracle)?;
        Ok(IOracle::getAllScurveDataReturn {
            bases,
            quotes,
            peakDays: peak_days,
            peakPrices: peak_prices,
        })
    })
}

fn all_scurve_data_for_pair(
    oracle: &OracleContract,
    c: IOracle::getAllScurveDataForPairCall,
) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_from(c.base, c.quote)?;
        let (peak_days, peak_prices) = crate::scurve::get_all_scurve_data_for_pair(oracle, pair)?;
        Ok(IOracle::getAllScurveDataForPairReturn {
            peakDays: peak_days,
            peakPrices: peak_prices,
        })
    })
}

fn nominal_price(oracle: &OracleContract, c: IOracle::getNominalPriceCall) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_from(c.base, c.quote)?;
        let (nominal, _, _, _) = oracle.get_nominal_price_components(pair, c.timestamp)?;
        Ok(nominal)
    })
}

fn nominal_price_components(
    oracle: &OracleContract,
    c: IOracle::getNominalPriceComponentsCall,
) -> Result<Bytes> {
    view(c, |c| {
        let pair = oracle.require_pair_from(c.base, c.quote)?;
        let (nominal_price, vwap, max_scurve, source) =
            oracle.get_nominal_price_components(pair, c.timestamp)?;
        Ok(IOracle::getNominalPriceComponentsReturn {
            nominalPrice: nominal_price,
            vwap,
            maxScurve: max_scurve,
            source,
        })
    })
}
