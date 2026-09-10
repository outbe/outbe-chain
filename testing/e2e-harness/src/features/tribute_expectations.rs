//! Tribute expectations from submitted amounts and historical Oracle inputs.

use alloy_primitives::{Address, U256};
use outbe_primitives::{addresses::ORACLE_ADDRESS, time::WorldwideDay};

use crate::{internal::eth, world::World};

pub(crate) fn amount_minor(base: &str, remainder: &str) -> U256 {
    let base: u64 = base.parse().expect("fixture whole amount");
    let remainder: u64 = remainder.parse().expect("fixture six-decimal remainder");
    assert!(remainder < 1_000_000);
    U256::from(base) * U256::from(1_000_000) + U256::from(remainder)
}

fn usd_nominal(amount: U256, vwap: U256, scurve: U256) -> (U256, U256) {
    assert!(!vwap.is_zero(), "available USD WorldwideDay price");
    let price = vwap.max(scurve);
    // Both currencies are USD, so their VWAP ratio cancels. Nominal is
    // COEN-equivalent, not the USD issuance amount. Keep the final floor.
    let nominal = amount
        .checked_mul(U256::from(1_000_000))
        .expect("fixture amount")
        / price;
    assert!(!nominal.is_zero());
    (nominal, price)
}

pub(crate) fn offer_height(world: &World, tx: &str) -> u64 {
    let receipt = eth::receipt_json(&world.rpc.url(world.validators.primary_port()), tx)
        .expect("submitted offer receipt");
    assert_eq!(receipt["status"], "0x1", "successful input offer");
    u64::from_str_radix(
        receipt["blockNumber"]
            .as_str()
            .expect("offer block")
            .trim_start_matches("0x"),
        16,
    )
    .expect("offer block quantity")
}

pub(crate) fn usd_offer_terms_at(
    world: &World,
    day: u32,
    height: u64,
    amount: U256,
) -> (U256, U256) {
    let ports = world.validators.committee_ports();
    world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 120)
        .expect("offer input finality");
    let usd = outbe_primitives::asset_type::currency_address(840);
    let mut expected = None;
    // These fixtures require stable pricing within the offer's block. This
    // avoids guessing an intra-block state from either a pre/post-block view.
    for checkpoint_height in [height.checked_sub(1).expect("non-genesis offer"), height] {
        let checkpoint = world
            .rpc
            .checkpoint_at(ports[0], checkpoint_height)
            .expect("pricing checkpoint");
        for &port in &ports {
            let snapshot = eth::read_call_at(
                &world.rpc.url(port),
                ORACLE_ADDRESS,
                &eth::IOracle::getWorldwideDayVwapSnapshotCall { worldwideDay: day },
                checkpoint_height,
            )
            .expect("input WWD prices");
            assert_eq!(snapshot.bases.len(), snapshot.quotes.len());
            assert_eq!(snapshot.bases.len(), snapshot.vwaps.len());
            let prices = snapshot
                .bases
                .iter()
                .zip(&snapshot.quotes)
                .zip(&snapshot.vwaps)
                .filter_map(|((base, quote), price)| {
                    (*base == Address::ZERO && *quote == usd).then_some(*price)
                })
                .collect::<Vec<_>>();
            assert_eq!(prices.len(), 1, "one USD input pair");
            let curves = eth::read_call_at(
                &world.rpc.url(port),
                ORACLE_ADDRESS,
                &eth::IOracle::getScurveValuesCall {
                    base: Address::ZERO,
                    quote: usd,
                    timestamp: WorldwideDay::new(day).to_timestamp_utc(),
                },
                checkpoint_height,
            )
            .expect("input S-curve values for the offer day");
            assert_eq!(curves.peakDays.len(), curves.peakPrices.len());
            assert_eq!(curves.peakDays.len(), curves.values.len());
            let terms = (
                prices[0],
                curves.values.into_iter().max().unwrap_or(U256::ZERO),
            );
            assert_eq!(
                *expected.get_or_insert(terms),
                terms,
                "stable input pricing on every validator"
            );
            assert_eq!(
                world
                    .rpc
                    .checkpoint_at(port, checkpoint_height)
                    .expect("recheck pricing checkpoint"),
                checkpoint
            );
        }
    }
    let (vwap, scurve) = expected.expect("input pricing");
    let result = usd_nominal(amount, vwap, scurve);
    eprintln!("TRIBUTE_INPUT_PRICING day={day} height={height} issuance={amount} vwap={vwap} scurve={scurve} nominal={} effective_price={}", result.0, result.1);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usd_nominal_keeps_price_normalization_and_floor() {
        assert_eq!(amount_minor("2", "80"), U256::from(2_000_080));
        for (amount, vwap, scurve, nominal, price) in [
            (2_000_000u64, 2u64, 999_960u64, 2_000_080u64, 999_960u64),
            (100_000_000, 500_000, 1_000_000, 100_000_000, 1_000_000),
            (2_000_000, 2, 0, 1_000_000_000_000, 2),
            (2_000_001, 700_000, 0, 2_857_144, 700_000),
        ] {
            assert_eq!(
                usd_nominal(U256::from(amount), U256::from(vwap), U256::from(scurve)),
                (U256::from(nominal), U256::from(price))
            );
        }
    }
}
