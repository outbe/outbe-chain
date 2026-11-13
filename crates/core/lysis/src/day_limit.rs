//! The Lysis Limit of an emission day.

use alloy_primitives::U256;

/// Share of the priced Tribute nominal that Lysis may load, in percent.
pub const SYMBOLIC_RATE: u64 = 32;

/// Returns `(demand, lysis_limit)`: `demand = floor(nominal * SYMBOLIC_RATE / 100)` and
/// `lysis_limit = min(demand, metadosis_limit)`, or `None` on overflow.
pub fn lysis_limit_of_day(nominal: U256, metadosis_limit: U256) -> Option<(U256, U256)> {
    let denominator = U256::from(100u64);
    let rate = U256::from(SYMBOLIC_RATE);
    let demand = (nominal / denominator)
        .checked_mul(rate)?
        .checked_add((nominal % denominator).checked_mul(rate)? / denominator)?;
    Some((demand, demand.min(metadosis_limit)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigUint;

    fn wide_floor_demand(nominal: U256) -> U256 {
        let wide = BigUint::from_bytes_be(&nominal.to_be_bytes::<32>());
        let bytes = (wide * SYMBOLIC_RATE / 100u32).to_bytes_be();
        assert!(bytes.len() <= 32, "wide demand exceeds U256");
        let mut padded = [0u8; 32];
        padded[32 - bytes.len()..].copy_from_slice(&bytes);
        U256::from_be_bytes(padded)
    }

    #[test]
    fn demand_matches_the_wide_oracle_and_lysis_respects_the_cap() {
        let nominals = [
            U256::ZERO,
            U256::from(99u64),
            U256::from(100u64),
            U256::from(101u64),
            U256::from(1u128) << 128,
            U256::MAX - U256::ONE,
            U256::MAX,
        ];
        let mut previous_demand = U256::ZERO;
        for nominal in nominals {
            let expected = wide_floor_demand(nominal);
            for cap in [U256::ZERO, U256::ONE, expected, U256::MAX] {
                let (demand, lysis_limit) = lysis_limit_of_day(nominal, cap).unwrap();
                assert_eq!(demand, expected, "nominal {nominal}");
                assert!(demand <= nominal, "nominal {nominal}");
                assert_eq!(
                    lysis_limit,
                    expected.min(cap),
                    "nominal {nominal} cap {cap}"
                );
            }
            assert!(expected >= previous_demand, "demand decreased at {nominal}");
            previous_demand = expected;
        }
    }
}
