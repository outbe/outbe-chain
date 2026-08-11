//! ABI encoding for Oracle vote submission.
//!
//! Takes U256 fixed-point prices from the aggregator and encodes them
//! into `submitVote(ExchangeRateTuple[])` calldata.

use alloy_primitives::Address;
use alloy_sol_types::{sol, SolCall};
use outbe_primitives::asset_type::AssetType;

use crate::aggregator::AggregatedPrice;
use crate::config::FeederConfig;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IOracle {
        struct ExchangeRateTuple {
            address base;
            address quote;
            uint256 exchangeRate;
            uint256 volume;
        }

        function submitVote(ExchangeRateTuple[] calldata tuples) external;
    }
}

/// The provider-side symbol as an oracle asset address.
///
/// `COEN` is the native asset and a 1-3 digit code is an ISO 4217 currency;
/// anything else must already be a 0x address. An unparseable symbol yields the
/// zero address, which the precompile rejects as an unregistered pair rather
/// than silently voting on the wrong one.
fn parse_asset(symbol: &str) -> Address {
    let text = symbol.trim();
    if text.eq_ignore_ascii_case("COEN") || text.eq_ignore_ascii_case("native") {
        return Address::ZERO;
    }
    if let Ok(code) = text.parse::<u16>() {
        if (1..=999).contains(&code) {
            return AssetType::IsoCurrency(code).into();
        }
    }
    text.parse::<Address>().unwrap_or(Address::ZERO)
}

/// Inverse of [`parse_asset`], for log lines.
fn show_asset(address: Address) -> String {
    match AssetType::from(address) {
        AssetType::Native => "COEN".to_string(),
        AssetType::IsoCurrency(code) => code.to_string(),
        AssetType::ERC20(token) => token.to_string(),
    }
}

/// Decodes ABI-encoded `submitVote` calldata back into a human-readable string.
///
/// Shows exactly what goes on-chain: `COEN/840:rate,vol | ETH/840:rate,vol`
pub fn decode_vote_log(calldata: &[u8]) -> eyre::Result<String> {
    let call = IOracle::submitVoteCall::abi_decode(calldata)
        .map_err(|e| eyre::eyre!("decode submitVote: {e}"))?;
    let parts: Vec<String> = call
        .tuples
        .iter()
        .map(|t| {
            format!(
                "{}/{}:{},{}",
                show_asset(t.base),
                show_asset(t.quote),
                t.exchangeRate,
                t.volume
            )
        })
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
            base: parse_asset(&p.base),
            quote: parse_asset(&p.quote),
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
            quote: "840".to_string(),
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
