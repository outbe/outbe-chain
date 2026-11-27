use alloy_primitives::U256;
use outbe_primitives::error::Result;

use crate::errors::EmissionLimitError;

/// The share of each agent pool in the Emission Limit, in percent. Metadosis takes the rest.
pub const VALIDATOR_REWARD_PCT: u64 = 4;
pub const WAA_REWARD_PCT: u64 = 4;
pub const SRA_REWARD_PCT: u64 = 4;
pub const CCA_REWARD_PCT: u64 = 4;

pub const PERCENT_DENOMINATOR: u64 = 100;

/// The limits of the parts of one Emission Limit, in protocol minor units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmissionShares {
    pub validators: U256,
    pub cca: U256,
    pub waa: U256,
    pub sra: U256,
    pub metadosis: U256,
}

impl EmissionShares {
    pub fn total(&self) -> Option<U256> {
        self.validators
            .checked_add(self.cca)?
            .checked_add(self.waa)?
            .checked_add(self.sra)?
            .checked_add(self.metadosis)
    }
}

/// Splits `limit`: each agent pool gets its percent rounded down, and Metadosis gets the rest.
pub fn compute_emission_shares(limit: U256) -> Result<EmissionShares> {
    let share = |pct: u64| -> Result<U256> {
        Ok(limit
            .checked_mul(U256::from(pct))
            .ok_or(EmissionLimitError::FixedShareMultiplicationOverflow)?
            / U256::from(PERCENT_DENOMINATOR))
    };
    let validators = share(VALIDATOR_REWARD_PCT)?;
    let cca = share(CCA_REWARD_PCT)?;
    let waa = share(WAA_REWARD_PCT)?;
    let sra = share(SRA_REWARD_PCT)?;
    // Four shares of 4% rounded down never exceed the limit.
    let metadosis = limit - validators - cca - waa - sra;
    Ok(EmissionShares {
        validators,
        cca,
        waa,
        sra,
        metadosis,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shares_give_the_limit_and_each_agent_share_is_four_percent_rounded_down() {
        for limit in [
            0u128,
            1,
            24,
            25,
            99,
            100,
            101,
            999_999,
            581_610_154_000_000,
            u128::MAX,
        ] {
            let limit = U256::from(limit);
            let shares = compute_emission_shares(limit).unwrap();
            let four_percent = limit * U256::from(4u64) / U256::from(100u64);
            assert_eq!(shares.validators, four_percent);
            assert_eq!(shares.cca, four_percent);
            assert_eq!(shares.waa, four_percent);
            assert_eq!(shares.sra, four_percent);
            assert_eq!(shares.total(), Some(limit));
        }
    }

    #[test]
    fn metadosis_takes_the_rounding_of_the_four_shares() {
        let shares = compute_emission_shares(U256::from(101u64)).unwrap();
        assert_eq!(shares.validators, U256::from(4u64));
        assert_eq!(shares.metadosis, U256::from(85u64));
    }

    #[test]
    fn the_shares_of_the_largest_day_are_pinned() {
        let limit = crate::day_emission::emission_limit_of_day(1_024);
        assert_eq!(limit, U256::from(581_610_154_000_000u64));
        let shares = compute_emission_shares(limit).unwrap();
        assert_eq!(shares.validators, U256::from(23_264_406_160_000u64));
        assert_eq!(shares.metadosis, U256::from(488_552_529_360_000u64));
    }
}
