//! Annual policy rates per ISO 4217 code.

use alloy_primitives::U256;
use outbe_primitives::error::Result;

use crate::errors::OracleError;
use crate::schema::OracleContract;

impl OracleContract<'_> {
    /// Annualized policy rate (scale `1e6`) for an independently registered
    /// ISO 4217 code. Reverts when no non-zero policy exists for the code.
    pub fn get_policy_rate(&self, iso_code: u16) -> Result<U256> {
        let rate = self.policy_rate.read(&iso_code)?;
        if rate.is_zero() {
            return Err(OracleError::NoPolicyRate { iso_code }.into());
        }
        Ok(rate)
    }
}
