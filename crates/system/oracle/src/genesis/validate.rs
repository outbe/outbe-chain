//! Validation of the genesis configuration before any state write.

use outbe_primitives::error::Result;

use super::OracleGenesisConfig;
use crate::constants::{MAX_SNAPSHOT_RETENTION_SECONDS, REFERENCE_CURRENCIES};
use crate::errors::OracleError;
use crate::schema::SCALE_1E18;

/// Checks the config parameters, then the rate vote targets, then that each
/// reference currency is a rate vote target.
pub(super) fn validate_genesis_config(config: &OracleGenesisConfig) -> Result<()> {
    if config.vote_period == 0 {
        return Err(OracleError::VotePeriodZero.into());
    }
    if config.slash_window == 0 {
        return Err(OracleError::SlashWindowZero.into());
    }
    if config.lookback_duration > MAX_SNAPSHOT_RETENTION_SECONDS {
        return Err(OracleError::LookbackExceedsRetention.into());
    }
    // 1e18-scaled ratios in [0, 1]. Above 1, slash_fraction gives a slash
    // percent above 100, and a large value overflows the u64 percent.
    for (field, value) in [
        ("reward_band", config.reward_band),
        ("gas_reward_band", config.gas_reward_band),
        ("min_valid_per_window", config.min_valid_per_window),
        ("slash_fraction", config.slash_fraction),
    ] {
        if value > SCALE_1E18 {
            return Err(OracleError::RatioAboveOne { field, value }.into());
        }
    }
    validate_rate_currencies(&config.policy_rate_currencies)?;
    // A Credis reservation reads the policy rate of its asset's currency, so
    // every reference currency needs a rate source.
    for iso_code in REFERENCE_CURRENCIES {
        if !config.policy_rate_currencies.contains(&iso_code) {
            return Err(OracleError::MissingPolicyRate { iso_code }.into());
        }
    }
    Ok(())
}

fn validate_rate_currencies(currencies: &[u16]) -> Result<()> {
    for (index, &iso_code) in currencies.iter().enumerate() {
        if iso_code == 0 {
            return Err(OracleError::PolicyIsoCodeZero.into());
        }
        if currencies[..index].contains(&iso_code) {
            return Err(OracleError::DuplicatePolicyIsoCode { iso_code }.into());
        }
    }
    Ok(())
}
