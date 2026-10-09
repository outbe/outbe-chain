//! Validation of the genesis configuration before any state write.

use outbe_primitives::error::Result;

use super::{OracleGenesisConfig, PolicyRate};
use crate::constants::{DAY_TYPE_ISO, MAX_SNAPSHOT_RETENTION_SECONDS};
use crate::errors::OracleError;

const MAX_REFERENCE_CURRENCIES: usize = 6;

/// Checks the config parameters, then the reference-currency and policy-rate
/// registries, in that order.
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
    validate_reference_currencies(&config.reference_currencies)?;
    validate_policy_rates(&config.policy_rates)
}

fn validate_reference_currencies(reference_currencies: &[u16]) -> Result<()> {
    if reference_currencies.len() > MAX_REFERENCE_CURRENCIES {
        return Err(OracleError::ReferenceCurrencyCountExceedsMax.into());
    }
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
