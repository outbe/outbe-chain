use crate::world::rpc::*;

impl Rpc {
    /// Read the exact WWD VWAP and the maximum active S-curve value for one
    /// COEN/ISO reference pair from the canonical Oracle precompile.
    pub fn oracle_wwd_vwap_and_scurve(
        &self,
        port: u16,
        worldwide_day: u32,
        iso_code: u16,
    ) -> Option<(U256, U256)> {
        let oracle = outbe_primitives::addresses::ORACLE_ADDRESS;
        let snapshot = eth::read_call(
            &self.url(port),
            oracle,
            &IOracle::getWorldwideDayVwapSnapshotCall {
                worldwideDay: worldwide_day,
            },
        )?;
        let quote = outbe_primitives::asset_type::currency_address(iso_code);
        let vwap = snapshot
            .bases
            .iter()
            .zip(&snapshot.quotes)
            .zip(&snapshot.vwaps)
            .find_map(|((base, candidate_quote), value)| {
                (*base == Address::ZERO && *candidate_quote == quote).then_some(*value)
            })?;
        let curve = eth::read_call(
            &self.url(port),
            oracle,
            &IOracle::getScurveValuesCall {
                base: Address::ZERO,
                quote,
                timestamp: outbe_primitives::time::date_key_to_utc_timestamp(worldwide_day),
            },
        )?;
        Some((vwap, curve.values.into_iter().max().unwrap_or(U256::ZERO)))
    }

    /// Read one canonical COEN/ISO rate together with its publication point.
    pub fn oracle_rate_data(&self, port: u16, iso_code: u16) -> Option<OracleRateDataV1> {
        self.oracle_rate_data_for_pair(
            port,
            Address::ZERO,
            outbe_primitives::asset_type::currency_address(iso_code),
        )
    }

    /// Read one configured Oracle pair together with its publication point.
    pub fn oracle_rate_data_for_pair(
        &self,
        port: u16,
        base: Address,
        quote: Address,
    ) -> Option<OracleRateDataV1> {
        let result = eth::read_call(
            &self.url(port),
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &IOracle::getExchangeRateDataCall { base, quote },
        )?;
        Some(OracleRateDataV1 {
            rate: result.rate,
            last_block: result.lastBlock,
            last_timestamp: result.lastTimestamp,
        })
    }

    /// Most recent published volume for one Oracle pair.
    pub fn oracle_latest_volume(&self, port: u16, base: Address, quote: Address) -> Option<U256> {
        let history = eth::read_call(
            &self.url(port),
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &IOracle::getPriceSnapshotHistoryCall {
                base,
                quote,
                count: 1,
            },
        )?;
        history.volumes.first().copied()
    }

    /// `(success, abstain, miss)` for one validator's current Oracle slash window.
    pub fn oracle_penalty_counts(&self, port: u16, validator: Address) -> Option<(u64, u64, u64)> {
        let progress = eth::read_call(
            &self.url(port),
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &IOracle::getSlashWindowProgressCall { validator },
        )?;
        Some((progress.success, progress.abstain, progress.miss))
    }

    /// Read the canonical chain-owned Oracle vote period used by production
    /// feeder preflight. Harness feeder config must match it exactly.
    pub fn oracle_vote_period(&self, port: u16) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &IOracle::getParamsCall {},
        )
        .map(|params| params.votePeriod)
    }
}

/// Latest canonical Oracle publication observed through a validator RPC.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct OracleRateDataV1 {
    pub rate: U256,
    pub last_block: u64,
    pub last_timestamp: u64,
}
