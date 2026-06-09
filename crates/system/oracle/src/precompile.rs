use crate::contract::OracleContract;
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolEvent, SolInterface};
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::dispatch::{dispatch_call, metadata, mutate_void, reject_value, view};
use outbe_primitives::error::Result;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IOracle.sol"
);

/// Dispatches an ABI-encoded call to the Oracle precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    dispatch_call(data, IOracle::IOracleCalls::abi_decode, |call| {
        let mut oracle = OracleContract::new(storage);
        use IOracle::IOracleCalls::*;
        match call {
            getExchangeRate(c) => view(c, |c| {
                let (rate, block, ts) = oracle.get_exchange_rate(&c.base, &c.quote)?;
                Ok((rate, block, ts).into())
            }),
            getVwap(c) => view(c, |c| {
                let pair_id = oracle.get_pair_id(&c.base, &c.quote)?;
                if pair_id == 0 {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "pair not registered".into(),
                    ));
                }
                let now = oracle.storage.timestamp()?.to::<u64>();
                oracle.calculate_vwap_lookback(pair_id, now, c.lookbackSeconds)
            }),
            getParams(_) => metadata::<IOracle::getParamsCall>(|| {
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
            }),
            getVotePenaltyCounter(c) => view(c, |c| {
                let success = oracle.penalty_success_count.read(&c.validator)?;
                let abstain = oracle.penalty_abstain_count.read(&c.validator)?;
                let miss = oracle.penalty_miss_count.read(&c.validator)?;
                Ok((success, abstain, miss).into())
            }),
            getFeederDelegation(c) => view(c, |c| oracle.get_feeder(&c.validator)),
            isVoteTarget(c) => view(c, |c| oracle.is_vote_target(&c.base, &c.quote)),
            getPairCount(_) => metadata::<IOracle::getPairCountCall>(|| oracle.pair_count.read()),
            getExchangeRates(_) => metadata::<IOracle::getExchangeRatesCall>(|| {
                let (rates, blocks, timestamps) = oracle.get_exchange_rates()?;
                Ok((rates, blocks, timestamps).into())
            }),
            getVoteTargets(_) => metadata::<IOracle::getVoteTargetsCall>(|| {
                let pair_ids = oracle.get_vote_targets()?;
                Ok(pair_ids)
            }),
            getAggregateVote(c) => view(c, |c| {
                let (exists, pair_ids, rates, volumes) = oracle.get_aggregate_vote(&c.validator)?;
                Ok((exists, pair_ids, rates, volumes).into())
            }),
            getSlashWindowProgress(c) => view(c, |c| {
                let (success, abstain, miss, slash_window) =
                    oracle.get_slash_window_progress(&c.validator)?;
                Ok((success, abstain, miss, slash_window).into())
            }),
            getVwapForTimeRange(c) => view(c, |c| {
                let pair_id = oracle.get_pair_id(&c.base, &c.quote)?;
                if pair_id == 0 {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "pair not registered".into(),
                    ));
                }
                oracle.calculate_vwap(pair_id, c.startTime, c.endTime)
            }),
            getScurveValue(c) => view(c, |c| {
                let pair_id = oracle.get_pair_id(&c.base, &c.quote)?;
                if pair_id == 0 {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "pair not registered".into(),
                    ));
                }
                crate::scurve::get_max_active_scurve_value(&oracle, pair_id, c.timestamp)
            }),
            setExchangeRate(c) => {
                reject_value(&value)?;
                let base = c.base.clone();
                let quote = c.quote.clone();
                let rate = c.rate;
                mutate_void(c, caller, |sender, c| {
                    // block_number and timestamp are not available in precompile context.
                    // Use 0 for bootstrap writes — tally will overwrite with real values.
                    oracle.set_exchange_rate(sender, &c.base, &c.quote, c.rate, 0, 0)?;
                    let event = IOracle::ExchangeRateSet { base, quote, rate };
                    let _ = oracle
                        .storage
                        .emit_event(ORACLE_ADDRESS, event.encode_log_data());
                    Ok(())
                })
            }
            delegateFeederConsent(c) => {
                reject_value(&value)?;
                let feeder = c.feeder;
                mutate_void(c, caller, |sender, c| {
                    oracle.delegate_feeder(sender, c.feeder)?;
                    let event = IOracle::FeederDelegated {
                        validator: sender,
                        feeder,
                    };
                    let _ = oracle
                        .storage
                        .emit_event(ORACLE_ADDRESS, event.encode_log_data());
                    Ok(())
                })
            }
            deactivateVoteTarget(c) => {
                reject_value(&value)?;
                let base = c.base.clone();
                let quote = c.quote.clone();
                mutate_void(c, caller, |sender, c| {
                    oracle.deactivate_vote_target(sender, &c.base, &c.quote)?;
                    let event = IOracle::VoteTargetDeactivated { base, quote };
                    let _ = oracle
                        .storage
                        .emit_event(ORACLE_ADDRESS, event.encode_log_data());
                    Ok(())
                })
            }
            activateVoteTarget(c) => {
                reject_value(&value)?;
                let base = c.base.clone();
                let quote = c.quote.clone();
                mutate_void(c, caller, |sender, c| {
                    oracle.activate_vote_target(sender, &c.base, &c.quote)?;
                    let event = IOracle::VoteTargetActivated { base, quote };
                    let _ = oracle
                        .storage
                        .emit_event(ORACLE_ADDRESS, event.encode_log_data());
                    Ok(())
                })
            }
            getPriceSnapshotHistory(c) => view(c, |c| {
                let pair_id = oracle.get_pair_id(&c.base, &c.quote)?;
                if pair_id == 0 {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "pair not registered".into(),
                    ));
                }
                let (timestamps, rates, volumes) =
                    oracle.get_price_snapshot_history(pair_id, c.count)?;
                Ok(IOracle::getPriceSnapshotHistoryReturn {
                    timestamps,
                    rates,
                    volumes,
                })
            }),
            getAllPriceSnapshotHistory(c) => view(c, |c| {
                let (snapshot_ids, timestamps, pair_ids, rates, volumes) =
                    oracle.get_all_price_snapshot_history(c.count)?;
                Ok(IOracle::getAllPriceSnapshotHistoryReturn {
                    snapshotIds: snapshot_ids,
                    timestamps,
                    pairIds: pair_ids,
                    rates,
                    volumes,
                })
            }),
            getTwap(c) => view(c, |c| {
                let pair_id = oracle.get_pair_id(&c.base, &c.quote)?;
                if pair_id == 0 {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "pair not registered".into(),
                    ));
                }
                let now = oracle.storage.timestamp()?.to::<u64>();
                oracle.calculate_twap(pair_id, now, c.lookbackSeconds)
            }),
            getTwaps(c) => view(c, |c| {
                let now = oracle.storage.timestamp()?.to::<u64>();
                let (pair_ids, twaps, lookbacks) = oracle.calculate_twaps(now, c.lookback)?;
                Ok(IOracle::getTwapsReturn {
                    pairIds: pair_ids,
                    twaps,
                    lookbackSeconds: lookbacks,
                })
            }),
            getDayVwap(c) => view(c, |c| {
                let pair_id = oracle.get_pair_id(&c.base, &c.quote)?;
                if pair_id == 0 {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "pair not registered".into(),
                    ));
                }
                let now = oracle.storage.timestamp()?.to::<u64>();
                oracle.calculate_vwap_lookback(pair_id, now, 86400)
            }),
            getWorldwideDayVwap(c) => view(c, |c| {
                let (pair_ids, vwaps, lookbacks) =
                    oracle.calculate_vwaps(c.startTime, c.endTime)?;
                Ok(IOracle::getWorldwideDayVwapReturn {
                    pairIds: pair_ids,
                    vwaps,
                    lookbackSeconds: lookbacks,
                })
            }),
            getWorldwideDayVwapSnapshot(c) => view(c, |c| {
                let (start_time, end_time, pair_ids, vwaps, lookbacks) =
                    oracle.get_worldwide_day_vwap_snapshot(c.worldwideDay.into())?;
                Ok(IOracle::getWorldwideDayVwapSnapshotReturn {
                    startTime: start_time,
                    endTime: end_time,
                    pairIds: pair_ids,
                    vwaps,
                    lookbackSeconds: lookbacks,
                })
            }),
            getScurveEntries(c) => view(c, |c| {
                let pair_id = oracle.get_pair_id(&c.base, &c.quote)?;
                if pair_id == 0 {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "pair not registered".into(),
                    ));
                }
                let now = oracle.storage.timestamp()?.to::<u64>();
                let (peak_days, peak_prices, current_values) =
                    crate::scurve::get_scurve_entries(&oracle, pair_id, now)?;
                Ok(IOracle::getScurveEntriesReturn {
                    peakDays: peak_days,
                    peakPrices: peak_prices,
                    currentValues: current_values,
                })
            }),
            getScurveValues(c) => view(c, |c| {
                let pair_id = oracle.get_pair_id(&c.base, &c.quote)?;
                if pair_id == 0 {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "pair not registered".into(),
                    ));
                }
                let target_day = crate::scurve::truncate_to_day(c.timestamp);
                let (peak_days, peak_prices, values) =
                    crate::scurve::get_scurve_entries(&oracle, pair_id, c.timestamp)?;
                Ok(IOracle::getScurveValuesReturn {
                    targetDay: target_day,
                    peakDays: peak_days,
                    peakPrices: peak_prices,
                    values,
                })
            }),
            getAllScurveData(_) => metadata::<IOracle::getAllScurveDataCall>(|| {
                let (pair_ids, peak_days, peak_prices) =
                    crate::scurve::get_all_scurve_data(&oracle)?;
                Ok(IOracle::getAllScurveDataReturn {
                    pairIds: pair_ids,
                    peakDays: peak_days,
                    peakPrices: peak_prices,
                })
            }),
            getAllScurveDataForPair(c) => view(c, |c| {
                let pair_id = oracle.get_pair_id(&c.base, &c.quote)?;
                if pair_id == 0 {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "pair not registered".into(),
                    ));
                }
                let (peak_days, peak_prices) =
                    crate::scurve::get_all_scurve_data_for_pair(&oracle, pair_id)?;
                Ok(IOracle::getAllScurveDataForPairReturn {
                    peakDays: peak_days,
                    peakPrices: peak_prices,
                })
            }),
            getPairs(_) => metadata::<IOracle::getPairsCall>(|| {
                let (pair_ids, bases, quotes, is_active) = oracle.get_pairs()?;
                Ok(IOracle::getPairsReturn {
                    pairIds: pair_ids,
                    bases,
                    quotes,
                    isActive: is_active,
                })
            }),
            getSettlementCurrency(c) => view(c, |c| {
                let denom_hash = oracle.settlement_iso_to_denom.read(&c.isoCode)?;
                let pair_hash = oracle.settlement_iso_to_pair.read(&c.isoCode)?;
                Ok((denom_hash, pair_hash).into())
            }),
            getSettlementCurrencies(_) => metadata::<IOracle::getSettlementCurrenciesCall>(|| {
                let (iso_codes, denoms, denom_hashes, pair_hashes) =
                    oracle.get_settlement_currencies()?;
                Ok(IOracle::getSettlementCurrenciesReturn {
                    isoCodes: iso_codes,
                    denoms,
                    denomHashes: denom_hashes,
                    pairHashes: pair_hashes,
                })
            }),
            getSettlementCount(_) => {
                metadata::<IOracle::getSettlementCountCall>(|| oracle.settlement_count.read())
            }
            getReferenceCurrencies(_) => metadata::<IOracle::getReferenceCurrenciesCall>(|| {
                oracle.reference_currencies.read_all()
            }),
            getNominalPrice(c) => view(c, |c| {
                let pair_id = oracle.get_pair_id(&c.base, &c.quote)?;
                if pair_id == 0 {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "pair not registered".into(),
                    ));
                }
                let (nominal, _, _, _) =
                    oracle.get_nominal_price_components(pair_id, c.timestamp)?;
                Ok(nominal)
            }),
            getNominalPriceComponents(c) => view(c, |c| {
                let pair_id = oracle.get_pair_id(&c.base, &c.quote)?;
                if pair_id == 0 {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "pair not registered".into(),
                    ));
                }
                let (nominal_price, vwap, max_scurve, source) =
                    oracle.get_nominal_price_components(pair_id, c.timestamp)?;
                Ok(IOracle::getNominalPriceComponentsReturn {
                    nominalPrice: nominal_price,
                    vwap,
                    maxScurve: max_scurve,
                    source,
                })
            }),
            submitVote(c) => {
                reject_value(&value)?;
                let tuple_count = c.tuples.len() as u32;
                mutate_void(c, caller, |sender, c| {
                    let tuples: Vec<_> = c
                        .tuples
                        .iter()
                        .map(|t| {
                            let hash = OracleContract::pair_hash(&t.base, &t.quote);
                            (hash, t.exchangeRate, t.volume)
                        })
                        .collect();
                    oracle.submit_vote(sender, &tuples)?;
                    // Emit event after successful vote
                    let event = IOracle::VoteSubmitted {
                        validator: sender,
                        tupleCount: tuple_count,
                    };
                    let _ = oracle
                        .storage
                        .emit_event(ORACLE_ADDRESS, event.encode_log_data());
                    Ok(())
                })
            }
        }
    })
}
