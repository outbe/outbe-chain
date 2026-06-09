//! Oracle commands.

use alloy_primitives::{Address, B256};
use alloy_sol_types::SolCall;
use clap::Subcommand;
use eyre::Result;

use crate::abi::{IOracle, ORACLE_ADDR};
use crate::rpc::Rpc;

#[derive(Subcommand)]
pub enum OracleCmd {
    /// Show exchange rate for a pair
    Rate {
        /// Base currency (e.g., COEN)
        base: String,
        /// Quote currency (e.g., 0xUSD)
        quote: String,
    },
    /// Show all exchange rates
    Rates,
    /// Show VWAP for a pair
    Vwap {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
        /// Lookback period in seconds (default: 86400)
        #[arg(default_value = "86400")]
        seconds: u64,
    },
    /// Show VWAP for a pair over an explicit time range
    VwapRange {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
        /// Start timestamp (seconds)
        start_time: u64,
        /// End timestamp (seconds)
        end_time: u64,
    },
    /// Show TWAP for a pair
    Twap {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
        /// Lookback period in seconds (default: 86400)
        #[arg(default_value = "86400")]
        seconds: u64,
    },
    /// Show TWAPs for all active vote-target pairs
    Twaps {
        /// Lookback period in seconds (default: 86400)
        #[arg(default_value = "86400")]
        seconds: u64,
    },
    /// Show day VWAP for a pair
    DayVwap {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
    },
    /// Show WorldwideDay-style VWAPs over an explicit time range
    WorldwideDayVwap {
        /// Start timestamp (seconds)
        start_time: u64,
        /// End timestamp (seconds)
        end_time: u64,
    },
    /// Show oracle parameters
    Params,
    /// Show registered pairs and vote targets
    Pairs,
    /// Show whether a pair is an active vote target
    IsVoteTarget {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
    },
    /// Show price snapshot history for a pair
    SnapshotHistory {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
        /// Maximum rows to return
        #[arg(long, default_value = "20")]
        count: u32,
    },
    /// Show flattened price snapshot history across all pairs
    AllSnapshotHistory {
        /// Maximum snapshots to return
        #[arg(long, default_value = "20")]
        count: u32,
    },
    /// Show penalty counters for a validator
    Penalty {
        /// Validator address
        validator: Address,
    },
    /// Show feeder delegation for a validator
    Feeder {
        /// Validator address
        validator: Address,
    },
    /// Show pending aggregate vote for a validator
    Vote {
        /// Validator address
        validator: Address,
    },
    /// Show S-curve value for a pair
    Scurve {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
        /// Timestamp to evaluate. Defaults to latest block timestamp.
        #[arg(long)]
        timestamp: Option<u64>,
    },
    /// Show active S-curve entries for a pair
    ScurveEntries {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
    },
    /// Show S-curve values for a pair at a timestamp
    ScurveValues {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
        /// Timestamp to evaluate
        timestamp: u64,
    },
    /// Show all S-curve data across pairs
    AllScurve,
    /// Show all S-curve data for a pair
    AllScurveForPair {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
    },
    /// Show S-curve adjusted nominal price for a pair
    NominalPrice {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
        /// Timestamp to evaluate. Defaults to latest block timestamp.
        #[arg(long)]
        timestamp: Option<u64>,
    },
    /// Show nominal price components for a pair
    NominalComponents {
        /// Base currency
        base: String,
        /// Quote currency
        quote: String,
        /// Timestamp to evaluate. Defaults to latest block timestamp.
        #[arg(long)]
        timestamp: Option<u64>,
    },
    /// Show settlement currency mapping for an ISO 4217 code
    Settlement {
        /// ISO 4217 numeric code
        iso_code: u16,
    },
    /// Show all settlement currencies
    Settlements,
    /// Show settlement currency count
    SettlementCount,
    /// Show vote target pair IDs
    VoteTargets,
    /// Show registered pair count
    PairCount,
    /// Delegate feeder consent to another address
    DelegateFeeder {
        /// Feeder address to delegate to
        feeder: Address,
    },
    /// Show slash window progress for a validator
    SlashProgress {
        /// Validator address
        validator: Address,
    },
}

impl OracleCmd {
    pub async fn run(self, client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
        match self {
            Self::Rate { base, quote } => rate(client, &base, &quote).await,
            Self::Rates => rates(client).await,
            Self::Vwap {
                base,
                quote,
                seconds,
            } => vwap(client, &base, &quote, seconds).await,
            Self::VwapRange {
                base,
                quote,
                start_time,
                end_time,
            } => vwap_range(client, &base, &quote, start_time, end_time).await,
            Self::Twap {
                base,
                quote,
                seconds,
            } => twap(client, &base, &quote, seconds).await,
            Self::Twaps { seconds } => twaps(client, seconds).await,
            Self::DayVwap { base, quote } => day_vwap(client, &base, &quote).await,
            Self::WorldwideDayVwap {
                start_time,
                end_time,
            } => worldwide_day_vwap(client, start_time, end_time).await,
            Self::Params => params(client).await,
            Self::Pairs => pairs(client).await,
            Self::IsVoteTarget { base, quote } => is_vote_target(client, &base, &quote).await,
            Self::SnapshotHistory { base, quote, count } => {
                snapshot_history(client, &base, &quote, count).await
            }
            Self::AllSnapshotHistory { count } => all_snapshot_history(client, count).await,
            Self::Penalty { validator } => penalty(client, validator).await,
            Self::Feeder { validator } => feeder(client, validator).await,
            Self::Vote { validator } => vote(client, validator).await,
            Self::Scurve {
                base,
                quote,
                timestamp,
            } => scurve(client, &base, &quote, timestamp).await,
            Self::ScurveEntries { base, quote } => scurve_entries(client, &base, &quote).await,
            Self::ScurveValues {
                base,
                quote,
                timestamp,
            } => scurve_values(client, &base, &quote, timestamp).await,
            Self::AllScurve => all_scurve(client).await,
            Self::AllScurveForPair { base, quote } => {
                all_scurve_for_pair(client, &base, &quote).await
            }
            Self::NominalPrice {
                base,
                quote,
                timestamp,
            } => nominal_price(client, &base, &quote, timestamp).await,
            Self::NominalComponents {
                base,
                quote,
                timestamp,
            } => nominal_components(client, &base, &quote, timestamp).await,
            Self::Settlement { iso_code } => settlement(client, iso_code).await,
            Self::Settlements => settlements(client).await,
            Self::SettlementCount => settlement_count(client).await,
            Self::VoteTargets => vote_targets(client).await,
            Self::PairCount => pair_count(client).await,
            Self::DelegateFeeder { feeder } => delegate_feeder(client, private_key, feeder).await,
            Self::SlashProgress { validator } => slash_progress(client, validator).await,
        }
    }
}

async fn rate(client: &(impl Rpc + Sync), base: &str, quote: &str) -> Result<()> {
    let call = IOracle::getExchangeRateCall {
        base: base.into(),
        quote: quote.into(),
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getExchangeRateCall::abi_decode_returns(&result)?;

    println!("=== Exchange Rate: {base}/{quote} ===");
    println!("Rate:      {} (1e18)", super::format_unit(ret.rate));
    println!("Block:     {}", ret.lastBlock);
    println!("Timestamp: {}", ret.lastTimestamp);
    Ok(())
}

async fn rates(client: &(impl Rpc + Sync)) -> Result<()> {
    let call = IOracle::getExchangeRatesCall {};
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getExchangeRatesCall::abi_decode_returns(&result)?;

    println!(
        "{:<6} {:<20} {:<12} {:<12}",
        "PairID", "Rate", "Block", "Timestamp"
    );
    println!("{}", "-".repeat(50));
    for (i, ((rate, block), ts)) in ret
        .rates
        .iter()
        .zip(ret.blocks.iter())
        .zip(ret.timestamps.iter())
        .enumerate()
    {
        println!(
            "{:<6} {:<20} {:<12} {:<12}",
            i + 1,
            super::format_unit(*rate),
            block,
            ts
        );
    }
    Ok(())
}

async fn vwap(client: &(impl Rpc + Sync), base: &str, quote: &str, seconds: u64) -> Result<()> {
    let call = IOracle::getVwapCall {
        base: base.into(),
        quote: quote.into(),
        lookbackSeconds: seconds,
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getVwapCall::abi_decode_returns(&result)?;
    println!(
        "VWAP {base}/{quote} ({}s lookback): {}",
        seconds,
        super::format_unit(ret)
    );
    Ok(())
}

async fn vwap_range(
    client: &(impl Rpc + Sync),
    base: &str,
    quote: &str,
    start_time: u64,
    end_time: u64,
) -> Result<()> {
    let call = IOracle::getVwapForTimeRangeCall {
        base: base.into(),
        quote: quote.into(),
        startTime: start_time,
        endTime: end_time,
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getVwapForTimeRangeCall::abi_decode_returns(&result)?;
    println!(
        "VWAP {base}/{quote} ({start_time}..{end_time}): {}",
        super::format_unit(ret)
    );
    Ok(())
}

async fn twap(client: &(impl Rpc + Sync), base: &str, quote: &str, seconds: u64) -> Result<()> {
    let call = IOracle::getTwapCall {
        base: base.into(),
        quote: quote.into(),
        lookbackSeconds: seconds,
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getTwapCall::abi_decode_returns(&result)?;
    println!(
        "TWAP {base}/{quote} ({}s lookback): {}",
        seconds,
        super::format_unit(ret)
    );
    Ok(())
}

async fn twaps(client: &(impl Rpc + Sync), seconds: u64) -> Result<()> {
    let call = IOracle::getTwapsCall {
        lookbackSeconds: seconds,
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getTwapsCall::abi_decode_returns(&result)?;

    println!("{:<8} {:<20} {:<12}", "PairID", "TWAP", "Lookback");
    println!("{}", "-".repeat(44));
    for ((pair_id, twap), lookback) in ret
        .pairIds
        .iter()
        .zip(ret.twaps.iter())
        .zip(ret.lookbackSeconds.iter())
    {
        println!(
            "{:<8} {:<20} {:<12}",
            pair_id,
            super::format_unit(*twap),
            lookback
        );
    }
    Ok(())
}

async fn day_vwap(client: &(impl Rpc + Sync), base: &str, quote: &str) -> Result<()> {
    let call = IOracle::getDayVwapCall {
        base: base.into(),
        quote: quote.into(),
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getDayVwapCall::abi_decode_returns(&result)?;
    println!("Day VWAP {base}/{quote}: {}", super::format_unit(ret));
    Ok(())
}

async fn worldwide_day_vwap(
    client: &(impl Rpc + Sync),
    start_time: u64,
    end_time: u64,
) -> Result<()> {
    let call = IOracle::getWorldwideDayVwapCall {
        startTime: start_time,
        endTime: end_time,
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getWorldwideDayVwapCall::abi_decode_returns(&result)?;

    println!("WorldwideDay VWAP window: {start_time}..{end_time}");
    println!("{:<8} {:<20} {:<12}", "PairID", "VWAP", "Lookback");
    println!("{}", "-".repeat(44));
    for ((pair_id, vwap), lookback) in ret
        .pairIds
        .iter()
        .zip(ret.vwaps.iter())
        .zip(ret.lookbackSeconds.iter())
    {
        println!(
            "{:<8} {:<20} {:<12}",
            pair_id,
            super::format_unit(*vwap),
            lookback
        );
    }
    Ok(())
}

async fn params(client: &(impl Rpc + Sync)) -> Result<()> {
    let call = IOracle::getParamsCall {};
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getParamsCall::abi_decode_returns(&result)?;

    println!("=== Oracle Parameters ===");
    println!("Vote Period:        {} blocks", ret.votePeriod);
    println!("Reward Band:        {}", super::format_unit(ret.rewardBand));
    println!("Slash Window:       {} blocks", ret.slashWindow);
    println!(
        "Min Valid/Window:   {}",
        super::format_unit(ret.minValidPerWindow)
    );
    println!(
        "Slash Fraction:     {}",
        super::format_unit(ret.slashFraction)
    );
    println!("Lookback Duration:  {}s", ret.lookbackDuration);
    println!("Enabled:            {}", ret.enabled);
    Ok(())
}

async fn pairs(client: &(impl Rpc + Sync)) -> Result<()> {
    let call = IOracle::getPairsCall {};
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getPairsCall::abi_decode_returns(&result)?;

    if ret.pairIds.is_empty() {
        println!("No oracle pairs registered.");
        return Ok(());
    }

    println!(
        "{:<8} {:<10} {:<10} {:<8}",
        "PairID", "Base", "Quote", "Active"
    );
    println!("{}", "-".repeat(40));
    for (((pair_id, base), quote), active) in ret
        .pairIds
        .iter()
        .zip(ret.bases.iter())
        .zip(ret.quotes.iter())
        .zip(ret.isActive.iter())
    {
        println!("{pair_id:<8} {base:<10} {quote:<10} {active:<8}");
    }
    Ok(())
}

async fn pair_count(client: &(impl Rpc + Sync)) -> Result<()> {
    let call = IOracle::getPairCountCall {};
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let count = IOracle::getPairCountCall::abi_decode_returns(&result)?;

    println!("Registered pairs: {count}");
    Ok(())
}

async fn vote_targets(client: &(impl Rpc + Sync)) -> Result<()> {
    let call = IOracle::getVoteTargetsCall {};
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let targets = IOracle::getVoteTargetsCall::abi_decode_returns(&result)?;

    println!("Active vote target pair IDs: {:?}", targets);
    Ok(())
}

async fn is_vote_target(client: &(impl Rpc + Sync), base: &str, quote: &str) -> Result<()> {
    let call = IOracle::isVoteTargetCall {
        base: base.into(),
        quote: quote.into(),
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let active = IOracle::isVoteTargetCall::abi_decode_returns(&result)?;

    println!("Vote target {base}/{quote}: {active}");
    Ok(())
}

async fn snapshot_history(
    client: &(impl Rpc + Sync),
    base: &str,
    quote: &str,
    count: u32,
) -> Result<()> {
    let call = IOracle::getPriceSnapshotHistoryCall {
        base: base.into(),
        quote: quote.into(),
        count,
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getPriceSnapshotHistoryCall::abi_decode_returns(&result)?;

    println!("=== Snapshot History: {base}/{quote} ===");
    println!("{:<12} {:<20} {:<20}", "Timestamp", "Rate", "Volume");
    println!("{}", "-".repeat(56));
    for ((timestamp, rate), volume) in ret
        .timestamps
        .iter()
        .zip(ret.rates.iter())
        .zip(ret.volumes.iter())
    {
        println!(
            "{:<12} {:<20} {:<20}",
            timestamp,
            super::format_unit(*rate),
            super::format_unit(*volume)
        );
    }
    Ok(())
}

async fn all_snapshot_history(client: &(impl Rpc + Sync), count: u32) -> Result<()> {
    let call = IOracle::getAllPriceSnapshotHistoryCall { count };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getAllPriceSnapshotHistoryCall::abi_decode_returns(&result)?;

    println!(
        "{:<10} {:<12} {:<8} {:<20} {:<20}",
        "Snapshot", "Timestamp", "PairID", "Rate", "Volume"
    );
    println!("{}", "-".repeat(78));
    for ((((snapshot_id, timestamp), pair_id), rate), volume) in ret
        .snapshotIds
        .iter()
        .zip(ret.timestamps.iter())
        .zip(ret.pairIds.iter())
        .zip(ret.rates.iter())
        .zip(ret.volumes.iter())
    {
        println!(
            "{:<10} {:<12} {:<8} {:<20} {:<20}",
            snapshot_id,
            timestamp,
            pair_id,
            super::format_unit(*rate),
            super::format_unit(*volume)
        );
    }
    Ok(())
}

async fn penalty(client: &(impl Rpc + Sync), validator: Address) -> Result<()> {
    let call = IOracle::getVotePenaltyCounterCall { validator };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getVotePenaltyCounterCall::abi_decode_returns(&result)?;

    println!("=== Penalty Counters for {validator} ===");
    println!("Success: {}", ret.success);
    println!("Abstain: {}", ret.abstain);
    println!("Miss:    {}", ret.miss);
    Ok(())
}

async fn feeder(client: &(impl Rpc + Sync), validator: Address) -> Result<()> {
    let call = IOracle::getFeederDelegationCall { validator };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getFeederDelegationCall::abi_decode_returns(&result)?;

    if ret == Address::ZERO {
        println!("Feeder for {validator}: self-delegation (no delegate)");
    } else {
        println!("Feeder for {validator}: {ret}");
    }
    Ok(())
}

async fn vote(client: &(impl Rpc + Sync), validator: Address) -> Result<()> {
    let call = IOracle::getAggregateVoteCall { validator };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getAggregateVoteCall::abi_decode_returns(&result)?;

    if !ret.exists {
        println!("No pending vote for {validator}");
        return Ok(());
    }

    println!("=== Aggregate Vote for {validator} ===");
    println!("{:<8} {:<20} {:<20}", "PairID", "Rate", "Volume");
    println!("{}", "-".repeat(48));
    for ((pid, rate), vol) in ret
        .pairIds
        .iter()
        .zip(ret.rates.iter())
        .zip(ret.volumes.iter())
    {
        println!(
            "{:<8} {:<20} {:<20}",
            pid,
            super::format_unit(*rate),
            super::format_unit(*vol)
        );
    }
    Ok(())
}

async fn scurve(
    client: &(impl Rpc + Sync),
    base: &str,
    quote: &str,
    timestamp: Option<u64>,
) -> Result<()> {
    let timestamp = resolve_timestamp(client, timestamp).await?;
    let call = IOracle::getScurveValueCall {
        base: base.into(),
        quote: quote.into(),
        timestamp,
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getScurveValueCall::abi_decode_returns(&result)?;
    println!(
        "S-curve value {base}/{quote} at {timestamp}: {}",
        super::format_unit(ret)
    );
    Ok(())
}

async fn scurve_entries(client: &(impl Rpc + Sync), base: &str, quote: &str) -> Result<()> {
    let call = IOracle::getScurveEntriesCall {
        base: base.into(),
        quote: quote.into(),
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getScurveEntriesCall::abi_decode_returns(&result)?;

    println!("=== Active S-curve Entries: {base}/{quote} ===");
    println!(
        "{:<12} {:<20} {:<20}",
        "PeakDay", "PeakPrice", "CurrentValue"
    );
    println!("{}", "-".repeat(58));
    for ((peak_day, peak_price), current_value) in ret
        .peakDays
        .iter()
        .zip(ret.peakPrices.iter())
        .zip(ret.currentValues.iter())
    {
        println!(
            "{:<12} {:<20} {:<20}",
            peak_day,
            super::format_unit(*peak_price),
            super::format_unit(*current_value)
        );
    }
    Ok(())
}

async fn scurve_values(
    client: &(impl Rpc + Sync),
    base: &str,
    quote: &str,
    timestamp: u64,
) -> Result<()> {
    let call = IOracle::getScurveValuesCall {
        base: base.into(),
        quote: quote.into(),
        timestamp,
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getScurveValuesCall::abi_decode_returns(&result)?;

    println!("Target day: {}", ret.targetDay);
    println!("{:<12} {:<20} {:<20}", "PeakDay", "PeakPrice", "Value");
    println!("{}", "-".repeat(58));
    for ((peak_day, peak_price), value) in ret
        .peakDays
        .iter()
        .zip(ret.peakPrices.iter())
        .zip(ret.values.iter())
    {
        println!(
            "{:<12} {:<20} {:<20}",
            peak_day,
            super::format_unit(*peak_price),
            super::format_unit(*value)
        );
    }
    Ok(())
}

async fn all_scurve(client: &(impl Rpc + Sync)) -> Result<()> {
    let call = IOracle::getAllScurveDataCall {};
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getAllScurveDataCall::abi_decode_returns(&result)?;

    println!("{:<8} {:<12} {:<20}", "PairID", "PeakDay", "PeakPrice");
    println!("{}", "-".repeat(44));
    for ((pair_id, peak_day), peak_price) in ret
        .pairIds
        .iter()
        .zip(ret.peakDays.iter())
        .zip(ret.peakPrices.iter())
    {
        println!(
            "{:<8} {:<12} {:<20}",
            pair_id,
            peak_day,
            super::format_unit(*peak_price)
        );
    }
    Ok(())
}

async fn all_scurve_for_pair(client: &(impl Rpc + Sync), base: &str, quote: &str) -> Result<()> {
    let call = IOracle::getAllScurveDataForPairCall {
        base: base.into(),
        quote: quote.into(),
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getAllScurveDataForPairCall::abi_decode_returns(&result)?;

    println!("=== All S-curve Data: {base}/{quote} ===");
    println!("{:<12} {:<20}", "PeakDay", "PeakPrice");
    println!("{}", "-".repeat(34));
    for (peak_day, peak_price) in ret.peakDays.iter().zip(ret.peakPrices.iter()) {
        println!("{:<12} {:<20}", peak_day, super::format_unit(*peak_price));
    }
    Ok(())
}

async fn nominal_price(
    client: &(impl Rpc + Sync),
    base: &str,
    quote: &str,
    timestamp: Option<u64>,
) -> Result<()> {
    let timestamp = resolve_timestamp(client, timestamp).await?;
    let call = IOracle::getNominalPriceCall {
        base: base.into(),
        quote: quote.into(),
        timestamp,
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getNominalPriceCall::abi_decode_returns(&result)?;
    println!(
        "Nominal price {base}/{quote} at {timestamp}: {}",
        super::format_unit(ret)
    );
    Ok(())
}

async fn nominal_components(
    client: &(impl Rpc + Sync),
    base: &str,
    quote: &str,
    timestamp: Option<u64>,
) -> Result<()> {
    let timestamp = resolve_timestamp(client, timestamp).await?;
    let call = IOracle::getNominalPriceComponentsCall {
        base: base.into(),
        quote: quote.into(),
        timestamp,
    };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getNominalPriceComponentsCall::abi_decode_returns(&result)?;

    println!("=== Nominal Price Components: {base}/{quote} at {timestamp} ===");
    println!("Nominal:   {}", super::format_unit(ret.nominalPrice));
    println!("VWAP:      {}", super::format_unit(ret.vwap));
    println!("MaxCurve:  {}", super::format_unit(ret.maxScurve));
    println!("Source:    {}", ret.source);
    Ok(())
}

async fn settlement(client: &(impl Rpc + Sync), iso_code: u16) -> Result<()> {
    let call = IOracle::getSettlementCurrencyCall { isoCode: iso_code };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getSettlementCurrencyCall::abi_decode_returns(&result)?;

    println!("Settlement currency {iso_code}:");
    println!("Denom Hash: {:?}", B256::from(ret.denomHash));
    println!("Pair Hash:  {:?}", B256::from(ret.pairHash));
    Ok(())
}

async fn settlements(client: &(impl Rpc + Sync)) -> Result<()> {
    let call = IOracle::getSettlementCurrenciesCall {};
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getSettlementCurrenciesCall::abi_decode_returns(&result)?;

    println!(
        "{:<8} {:<16} {:<66} {:<66}",
        "ISO", "Denom", "DenomHash", "PairHash"
    );
    println!("{}", "-".repeat(160));
    for (((iso, denom), denom_hash), pair_hash) in ret
        .isoCodes
        .iter()
        .zip(ret.denoms.iter())
        .zip(ret.denomHashes.iter())
        .zip(ret.pairHashes.iter())
    {
        println!(
            "{:<8} {:<16} {:?} {:?}",
            iso,
            denom,
            B256::from(*denom_hash),
            B256::from(*pair_hash)
        );
    }
    Ok(())
}

async fn settlement_count(client: &(impl Rpc + Sync)) -> Result<()> {
    let call = IOracle::getSettlementCountCall {};
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let count = IOracle::getSettlementCountCall::abi_decode_returns(&result)?;

    println!("Settlement currencies: {count}");
    Ok(())
}

async fn delegate_feeder(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    feeder: Address,
) -> Result<()> {
    let signer = super::require_signer(private_key)?;
    let call = IOracle::delegateFeederConsentCall { feeder };
    let tx_hash = signer
        .send_tx(client, ORACLE_ADDR, call.abi_encode(), Default::default())
        .await?;
    println!("Feeder delegation tx sent: {tx_hash}");
    Ok(())
}

async fn slash_progress(client: &(impl Rpc + Sync), validator: Address) -> Result<()> {
    let call = IOracle::getSlashWindowProgressCall { validator };
    let result = client.eth_call(ORACLE_ADDR, &call.abi_encode()).await?;
    let ret = IOracle::getSlashWindowProgressCall::abi_decode_returns(&result)?;

    println!("=== Slash Window Progress for {validator} ===");
    println!("Success:      {}", ret.success);
    println!("Abstain:      {}", ret.abstain);
    println!("Miss:         {}", ret.miss);
    println!("Slash Window: {} blocks", ret.slashWindow);

    let total = ret
        .success
        .saturating_add(ret.abstain)
        .saturating_add(ret.miss);
    if total > 0 {
        println!("Valid Rate:   {}", format_percent(ret.success, total));
    }
    Ok(())
}

async fn resolve_timestamp(client: &(impl Rpc + Sync), timestamp: Option<u64>) -> Result<u64> {
    if let Some(timestamp) = timestamp {
        return Ok(timestamp);
    }

    let latest = client.eth_get_latest_block().await?;
    latest
        .get("timestamp")
        .and_then(|value| value.as_str())
        .ok_or_else(|| eyre::eyre!("latest block response is missing timestamp"))
        .and_then(parse_hex_u64)
}

fn parse_hex_u64(value: &str) -> Result<u64> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    u64::from_str_radix(value, 16).map_err(|e| eyre::eyre!("failed to parse timestamp: {e}"))
}

fn format_percent(numerator: u64, denominator: u64) -> String {
    if denominator == 0 {
        return "n/a".to_string();
    }

    let basis_points = u128::from(numerator) * 10_000 / u128::from(denominator);
    let whole = basis_points / 100;
    let frac = basis_points % 100;
    format!("{whole}.{frac:02}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oracle_cmd_parse() {
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: OracleCmd,
        }

        let cli = TestCli::try_parse_from(["test", "params"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "rate", "COEN", "0xUSD"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "rates"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "vwap", "COEN", "0xUSD", "3600"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "vwap-range", "COEN", "0xUSD", "100", "200"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "twap", "COEN", "0xUSD", "3600"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "twaps", "3600"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "day-vwap", "COEN", "0xUSD"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "worldwide-day-vwap", "100", "200"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "pairs"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "pair-count"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "vote-targets"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "is-vote-target", "COEN", "0xUSD"]);
        assert!(cli.is_ok());

        let cli =
            TestCli::try_parse_from(["test", "snapshot-history", "COEN", "0xUSD", "--count", "5"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "all-snapshot-history", "--count", "5"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from([
            "test",
            "penalty",
            "0x1111111111111111111111111111111111111111",
        ]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "scurve", "COEN", "0xUSD"]);
        assert!(cli.is_ok());

        let cli =
            TestCli::try_parse_from(["test", "scurve", "COEN", "0xUSD", "--timestamp", "123"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "scurve-entries", "COEN", "0xUSD"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "scurve-values", "COEN", "0xUSD", "123"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "all-scurve"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "all-scurve-for-pair", "COEN", "0xUSD"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "nominal-price", "COEN", "0xUSD"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from([
            "test",
            "nominal-components",
            "COEN",
            "0xUSD",
            "--timestamp",
            "123",
        ]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "settlement", "840"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "settlements"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from(["test", "settlement-count"]);
        assert!(cli.is_ok());

        let cli = TestCli::try_parse_from([
            "test",
            "delegate-feeder",
            "0x1111111111111111111111111111111111111111",
        ]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_format_percent_uses_integer_math() {
        assert_eq!(format_percent(1, 3), "33.33%");
        assert_eq!(format_percent(0, 0), "n/a");
    }

    #[test]
    fn test_parse_hex_u64_timestamp() {
        assert_eq!(parse_hex_u64("0x7b").unwrap(), 123);
        assert_eq!(parse_hex_u64("7b").unwrap(), 123);
        assert!(parse_hex_u64("0xnot-hex").is_err());
    }
}
