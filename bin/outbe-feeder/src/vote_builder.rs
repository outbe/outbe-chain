//! ABI encoding for Oracle vote submission.
//!
//! Takes U256 fixed-point prices from the aggregator and encodes them
//! into `submitVote(ExchangeRateTuple[])` calldata.

use alloy_sol_types::{sol, SolCall};

use crate::aggregator::AggregatedPrice;
use crate::config::FeederConfig;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IOracle {
        struct ExchangeRateTuple {
            string base;
            string quote;
            uint256 exchangeRate;
            uint256 volume;
        }

        function submitVote(ExchangeRateTuple[] calldata tuples) external;
    }
}

/// Decodes ABI-encoded `submitVote` calldata back into a human-readable string.
///
/// Shows exactly what goes on-chain: `COEN/0xUSD:rate,vol | ETH/0xUSD:rate,vol`
pub fn decode_vote_log(calldata: &[u8]) -> eyre::Result<String> {
    let call = IOracle::submitVoteCall::abi_decode(calldata)
        .map_err(|e| eyre::eyre!("decode submitVote: {e}"))?;
    let parts: Vec<String> = call
        .tuples
        .iter()
        .map(|t| format!("{}/{}:{},{}", t.base, t.quote, t.exchangeRate, t.volume))
        .collect();
    Ok(parts.join(" | "))
}

/// Encodes aggregated prices into ABI-encoded `submitVote` calldata.
///
/// Prices and volumes are already U256 at 1e18 scale from the aggregator.
pub fn encode_vote(prices: &[AggregatedPrice], _config: &FeederConfig) -> Vec<u8> {
    let tuples: Vec<IOracle::ExchangeRateTuple> = prices
        .iter()
        .map(|p| IOracle::ExchangeRateTuple {
            base: p.base.clone(),
            quote: p.quote.clone(),
            exchangeRate: p.price,
            volume: p.volume,
        })
        .collect();

    let call = IOracle::submitVoteCall { tuples };
    call.abi_encode()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use outbe_primitives::units::{Units, SCALE_1E18};

    #[test]
    fn test_encode_vote_produces_calldata() {
        let prices = vec![AggregatedPrice {
            base: "COEN".to_string(),
            quote: "0xUSD".to_string(),
            price: SCALE_1E18 + SCALE_1E18 / U256::from(2u64), // 1.5
            volume: U256::in_units(10000u128),
        }];
        let config = crate::config::FeederConfig {
            chain: crate::config::ChainConfig {
                rpc_endpoint: "http://localhost:8545".to_string(),
                chain_id: 31337,
                gasless_oracle_votes: false,
            },
            account: crate::config::AccountConfig {
                private_key: "0x".to_string(),
                validator_address: "0x".to_string(),
            },
            oracle: crate::config::OracleConfig {
                vote_period: 2,
                poll_interval_secs: 2,
            },
            currency_pairs: vec![],
            deviation_thresholds: vec![],
            provider_endpoints: vec![],
            health: None,
        };
        let calldata = encode_vote(&prices, &config);
        // submitVote selector is first 4 bytes
        assert!(calldata.len() > 4);
    }
}
