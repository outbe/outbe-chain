//! Validator liveness, TEE lease deadlines, and enclave-upgrade sweeps.

use outbe_primitives::block::{BlockRuntimeContext, ProtocolModule};
use outbe_primitives::error::Result;
use outbe_primitives::tee_attestation_v1::TEE_ATTESTATION_V1_ACTIVATION_HEIGHT;
use outbe_teeregistry::TeeRegistry;
use outbe_validatorset::contract::ValidatorSet;

use crate::errors::LivenessError;
use crate::schema::SlashIndicator;

/// Protocol module for the enclave-upgrade deadline system transaction.
pub struct EnclaveUpgradeDeadline;

impl ProtocolModule for EnclaveUpgradeDeadline {
    const NAME: &'static str = "enclave_upgrade_deadline";

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        enforce_enclave_upgrade_deadline(ctx)
    }
}

/// Runs the validator-liveness consensus step.
pub struct ValidatorLivenessModule;

impl ProtocolModule for ValidatorLivenessModule {
    const NAME: &'static str = "validator_liveness";

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        run_validator_liveness(ctx)
    }
}

/// Records the block proposer, then jails validators whose TEE lease has ended.
pub fn run_validator_liveness(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    record_cycle_proposer(ctx)?;
    enforce_tee_lease_deadlines(ctx)?;
    Ok(())
}

/// Jails and slashes active validators whose enclave lease is due.
///
/// Block 1 is skipped. Its bootstrap transaction creates the founder bindings.
/// One storage checkpoint holds every jail and slash. Metrics are published
/// only after that checkpoint commits.
pub fn enforce_tee_lease_deadlines(ctx: &BlockRuntimeContext<'_>) -> Result<usize> {
    if ctx.block.block_number <= TEE_ATTESTATION_V1_ACTIVATION_HEIGHT {
        return Ok(0);
    }
    let overdue = overdue_lease_validators(ctx)?;
    if overdue.is_empty() {
        return Ok(0);
    }
    let transitioned = ctx
        .storage
        .with_checkpoint(|| jail_overdue_leases(ctx, &overdue))?;
    publish_tee_expiry(&transitioned, ctx.block.block_number);
    Ok(transitioned.len())
}

/// Jails validators that missed the enclave-upgrade deadline.
///
/// The sweep commits in one checkpoint. Punishment reports are recorded only
/// after that checkpoint returns.
pub fn enforce_enclave_upgrade_deadline(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let mut registry = TeeRegistry::new(ctx.storage.clone());
    let Some(upgrade) = registry.upgrade_sweep_due_v1()? else {
        return Ok(());
    };
    let mut reports = Vec::new();
    ctx.with_checkpoint(|| {
        let slash_percent = SlashIndicator::new(ctx.storage.clone()).slash_amount_percent()?;
        let mut validators = ValidatorSet::new(ctx.storage.clone());
        let active = validators.get_active_validators()?;
        for validator in active {
            let address = validator.validator_address;
            if upgrade_already_satisfied(&registry, &upgrade, address)? {
                continue;
            }
            if let Some(report) = validators.jail_validator_deferred(address)? {
                reports.push(report);
            }
            let slashed = staking_slash(ctx, address, slash_percent)?;
            registry.mark_upgrade_penalty_v1(upgrade.proposal_id, address)?;
            registry.emit(
                outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1::EnclaveUpgradeMissedV1 {
                    proposalId: upgrade.proposal_id,
                    validator: address,
                    activationHeight: upgrade.activation_height,
                    requiredMrenclave: upgrade.mrenclave,
                    slashedAmount: slashed,
                },
            )?;
        }
        registry.mark_upgrade_swept_v1(upgrade.proposal_id)
    })?;
    for report in reports {
        report.record();
    }
    Ok(())
}

fn record_cycle_proposer(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let mut validator_set = ValidatorSet::new(ctx.storage.clone());
    if validator_set.is_consensus_participant(ctx.block.proposer)? {
        validator_set.record_proposer(ctx.block.proposer)?;
        return Ok(());
    }
    Err(LivenessError::ProposerNotConsensusParticipant {
        proposer: ctx.block.proposer,
    }
    .into())
}

fn overdue_lease_validators(
    ctx: &BlockRuntimeContext<'_>,
) -> Result<Vec<alloy_primitives::Address>> {
    let active = ValidatorSet::new(ctx.storage.clone()).get_active_validators()?;
    let registry = TeeRegistry::new(ctx.storage.clone());
    let mut overdue = Vec::new();
    for validator in active {
        let binding = registry.validator_enclave_binding_v1(validator.validator_address)?;
        if binding.is_none_or(|binding| binding.valid_until <= ctx.block.timestamp) {
            overdue.push(validator.validator_address);
        }
    }
    Ok(overdue)
}

fn jail_overdue_leases(
    ctx: &BlockRuntimeContext<'_>,
    overdue: &[alloy_primitives::Address],
) -> Result<Vec<alloy_primitives::Address>> {
    let percent = SlashIndicator::new(ctx.storage.clone()).slash_amount_percent()?;
    ctx.storage.now_secs()?;
    let mut validator_set = ValidatorSet::new(ctx.storage.clone());
    let mut staking = outbe_staking::contract::Staking::new(ctx.storage.clone());
    let mut transitioned = Vec::new();
    for validator in overdue {
        if validator_set.jail_validator_for_tee_expiry(*validator)? {
            staking.slash_stake(*validator, percent)?;
            transitioned.push(*validator);
        }
    }
    Ok(transitioned)
}

fn publish_tee_expiry(transitioned: &[alloy_primitives::Address], block_number: u64) {
    for validator in transitioned {
        outbe_validatorset::metrics::record_validator_status(
            *validator,
            outbe_validatorset::runtime::status::JAILED,
        );
        outbe_validatorset::metrics::record_validator_tee_expiry(*validator, "deadline_jailed");
        outbe_validatorset::metrics::record_pending_set_change(true);
        tracing::warn!(
            target: "outbe::validatorset",
            event = "validator_tee_lease_expired",
            validator = %validator,
            block_number,
            "validator jailed and slashed after TEE lease deadline",
        );
    }
}

fn upgrade_already_satisfied(
    registry: &TeeRegistry<'_>,
    upgrade: &outbe_teeregistry::upgrade::EnclaveUpgradeV1,
    address: alloy_primitives::Address,
) -> Result<bool> {
    let updated = registry
        .validator_enclave_binding_v1(address)?
        .is_some_and(|binding| {
            binding.policy_hash == upgrade.successor_policy_hash
                && binding.mrenclave == upgrade.mrenclave
        });
    if updated {
        return Ok(true);
    }
    registry.upgrade_penalty_applied_v1(upgrade.proposal_id, address)
}

fn staking_slash(
    ctx: &BlockRuntimeContext<'_>,
    address: alloy_primitives::Address,
    slash_percent: u64,
) -> Result<alloy_primitives::U256> {
    outbe_staking::contract::Staking::new(ctx.storage.clone()).slash_stake(address, slash_percent)
}

#[cfg(test)]
mod tests {
    use super::enforce_tee_lease_deadlines;
    use alloy_primitives::{Address, B256};
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;
    use outbe_primitives::tee_attestation_v1::TEE_ATTESTATION_V1_ACTIVATION_HEIGHT;

    #[test]
    fn bootstrap_block_skips_the_lease_sweep() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_number: TEE_ATTESTATION_V1_ACTIVATION_HEIGHT,
                    timestamp: 10,
                    chain_id: 1,
                    genesis_hash: B256::ZERO,
                    genesis_timestamp: 0,
                    proposer: Address::ZERO,
                    validators: Vec::new(),
                    block_gas_limit: 0,
                    user_gas_used: 0,
                },
                storage,
            );
            assert_eq!(enforce_tee_lease_deadlines(&ctx).unwrap(), 0);
        });
    }
}
