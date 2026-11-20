//! Policy rates: the last closed rate of each currency and its publication time.

use alloy_primitives::U256;
use outbe_primitives::error::Result;

use crate::errors::OracleError;
use crate::schema::OracleContract;

impl OracleContract<'_> {
    /// The last closed annual policy rate (scale `1e6`) of `iso_code` and the
    /// time at which its source published it.
    ///
    /// A rate of zero is a rate. Returns [`OracleError::NoPolicyRate`] when no
    /// tally closed a rate for the currency: its publication time is zero.
    pub fn get_policy_rate(&self, iso_code: u16) -> Result<(U256, u64)> {
        let published_at = self.policy_rate_published_at.read(&iso_code)?;
        if published_at == 0 {
            return Err(OracleError::NoPolicyRate { iso_code }.into());
        }
        Ok((self.policy_rate.read(&iso_code)?, published_at))
    }

    /// Writes a closed rate without a tally. For tests of other modules only:
    /// no call of the chain sets a rate.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn set_policy_rate_for_test(
        &mut self,
        iso_code: u16,
        rate: U256,
        published_at: u64,
    ) -> Result<()> {
        self.policy_rate.write(&iso_code, rate)?;
        self.policy_rate_published_at.write(&iso_code, published_at)
    }
}
