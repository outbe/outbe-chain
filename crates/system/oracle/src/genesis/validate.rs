//! Validation of the genesis configuration before any state write.

use outbe_primitives::error::Result;

use super::{OracleGenesisConfig, PolicyRate};
use crate::constants::{DAY_TYPE_ISO, MAX_SNAPSHOT_RETENTION_SECONDS};
use crate::errors::OracleError;
use crate::schema::SCALE_1E18;

/// Checks the config parameters, then the reference-currency and policy-rate
/// registries, in that order, then that each reference currency has a policy rate.
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
        ("min_valid_per_window", config.min_valid_per_window),
        ("slash_fraction", config.slash_fraction),
    ] {
        if value > SCALE_1E18 {
            return Err(OracleError::RatioAboveOne { field, value }.into());
        }
    }
    validate_reference_currencies(&config.reference_currencies)?;
    validate_policy_rates(&config.policy_rates)?;
    // A Credis reservation reads the policy rate of its asset's currency, so
    // every reference currency needs one.
    for iso_code in &config.reference_currencies {
        if !config
            .policy_rates
            .iter()
            .any(|policy| policy.iso_code == *iso_code)
        {
            return Err(OracleError::MissingPolicyRate {
                iso_code: *iso_code,
            }
            .into());
        }
    }
    Ok(())
}

fn validate_reference_currencies(reference_currencies: &[u16]) -> Result<()> {
    let mut previous = None;
    for iso_code in reference_currencies {
        if *iso_code == 0 {
            return Err(OracleError::ReferenceIsoCodeZero.into());
        }
        if let Some(previous) = previous {
            if *iso_code == previous {
                return Err(OracleError::DuplicateReferenceIsoCode {
                    iso_code: *iso_code,
                }
                .into());
            }
            if *iso_code < previous {
                return Err(OracleError::ReferenceCurrenciesNotSorted.into());
            }
        }
        previous = Some(*iso_code);
    }
    if reference_currencies.binary_search(&DAY_TYPE_ISO).is_err() {
        return Err(OracleError::MissingUsdReferenceCurrency.into());
    }
    Ok(())
}

fn validate_policy_rates(policy_rates: &[PolicyRate]) -> Result<()> {
    let mut previous = None;
    for policy in policy_rates {
        if policy.iso_code == 0 {
            return Err(OracleError::PolicyIsoCodeZero.into());
        }
        if policy.annual_rate_1e6.is_zero() {
            return Err(OracleError::PolicyRateZero {
                iso_code: policy.iso_code,
            }
            .into());
        }
        if let Some(previous) = previous {
            if policy.iso_code == previous {
                return Err(OracleError::DuplicatePolicyIsoCode {
                    iso_code: policy.iso_code,
                }
                .into());
            }
            if policy.iso_code < previous {
                return Err(OracleError::PolicyRatesNotSorted.into());
            }
        }
        previous = Some(policy.iso_code);
    }
    Ok(())
}
