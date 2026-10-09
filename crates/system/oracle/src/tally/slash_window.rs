//! Slash window: vote-rate check, jail and slash of underperformers, and
//! penalty counter reset.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::error::Result;

use super::MAX_ORACLE_SLASH_WINDOW_VALIDATORS;
use crate::errors::OracleError;
use crate::precompile::IOracle;
use crate::schema::{OracleContract, SCALE_1E18};

/// Slash-window configuration read once per window.
#[derive(Clone, Copy, Debug)]
struct SlashPolicy {
    /// Minimum `success / total` vote rate at scale `1e18`.
    min_valid: U256,
    /// `true` when protected validators skip the vote-rate check.
    allow_protected: bool,
}

/// Processes the slash window: checks vote rates and jails underperformers.
pub fn slash_and_reset_counters(oracle: &mut OracleContract, _timestamp: u64) -> Result<()> {
    let policy = SlashPolicy {
        min_valid: oracle.config_min_valid_per_window.read()?,
        allow_protected: oracle.config_allow_protected.read()?,
    };

    let vs = outbe_validatorset::contract::ValidatorSet::new(oracle.storage.clone());
    let validator_addresses = vs.registered_validator_addresses()?;
    if validator_addresses.len() > MAX_ORACLE_SLASH_WINDOW_VALIDATORS {
        return Err(OracleError::SlashWindowValidatorSetExceedsCap {
            actual: validator_addresses.len(),
            cap: MAX_ORACLE_SLASH_WINDOW_VALIDATORS,
        }
        .into());
    }

    for addr in validator_addresses {
        close_validator_window(oracle, addr, policy)?;
    }

    // Remove exchange rates for deactivated pairs (Cosmos: RemoveExcessFeeds)
    oracle.remove_excess_feeds()?;

    Ok(())
}

/// Closes the slash window of one validator. A protected validator, a
/// validator without rounds, and a validator at or above the minimum vote rate
/// only get a counter reset. Any other validator is jailed and slashed first.
fn close_validator_window(
    oracle: &mut OracleContract,
    addr: Address,
    policy: SlashPolicy,
) -> Result<()> {
    // Skip protected validators
    if policy.allow_protected && oracle.protected_validator.read(&addr)? {
        return oracle.reset_penalty_counter(&addr);
    }

    let success = oracle.penalty_success_count.read(&addr)?;
    let abstain = oracle.penalty_abstain_count.read(&addr)?;
    let miss = oracle.penalty_miss_count.read(&addr)?;
    let total = success + abstain + miss;

    if total == 0 {
        return oracle.reset_penalty_counter(&addr);
    }

    // valid_rate = success * 1e18 / total
    let valid_rate = U256::from(success) * SCALE_1E18 / U256::from(total);

    if valid_rate < policy.min_valid {
        return jail_and_slash(oracle, addr);
    }

    oracle.reset_penalty_counter(&addr)
}

/// Jails `addr`, slashes its stake by the configured fraction, and resets its
/// penalty counters in one storage checkpoint.
fn jail_and_slash(oracle: &mut OracleContract, addr: Address) -> Result<()> {
    let storage = oracle.storage.clone();
    storage.with_checkpoint(|| {
        // Jail first. The penalty is jail plus slash, not a force-exit.
        // A later failure rolls the jail storage write back with this
        // checkpoint. `jail_validator` records metrics before that
        // rollback. Those metrics stay outside the checkpoint.
        let mut vs_mut = outbe_validatorset::contract::ValidatorSet::new(oracle.storage.clone());
        // Oracle underperformance felony: JAIL (not force-exit) + slash.
        vs_mut.jail_validator(addr)?;
        let event = IOracle::ValidatorForcedExit { validator: addr };
        let _ = oracle
            .storage
            .emit_event(ORACLE_ADDRESS, event.encode_log_data());

        slash_stake(oracle, addr)?;

        oracle.reset_penalty_counter(&addr)?;
        Ok(())
    })
}

/// Slashes the stake of `addr` by the configured slash fraction, rounded down
/// to a whole percent. A zero percent slashes nothing and emits no event.
fn slash_stake(oracle: &mut OracleContract, addr: Address) -> Result<()> {
    let slash_fraction = oracle.config_slash_fraction.read()?;
    if slash_fraction.is_zero() {
        return Ok(());
    }
    // Convert 1e18-scaled fraction to percent: fraction * 100 / 1e18
    let slash_pct = (slash_fraction * U256::from(100u64) / SCALE_1E18).to::<u64>();
    if slash_pct > 0 {
        let mut staking = outbe_staking::contract::Staking::new(oracle.storage.clone());
        staking.slash_stake(addr, slash_pct)?;
        let event = IOracle::ValidatorSlashed {
            validator: addr,
            slashPercent: slash_pct,
        };
        let _ = oracle
            .storage
            .emit_event(ORACLE_ADDRESS, event.encode_log_data());
    }
    Ok(())
}
