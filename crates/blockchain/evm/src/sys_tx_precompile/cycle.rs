use outbe_primitives::block::{BlockRuntimeContext, ProtocolModule};
use outbe_primitives::error::Result;

/// Protocol module for `SystemTxKind::EnclaveUpgradeDeadline` (`OSEU`).
pub struct EnclaveUpgradeDeadline;

impl ProtocolModule for EnclaveUpgradeDeadline {
    const NAME: &'static str = "enclave_upgrade_deadline";

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        enforce_enclave_upgrade_deadline(ctx)
    }
}

pub(crate) fn run_validator_liveness(ctx: &BlockRuntimeContext) -> Result<()> {
    validate_and_record_cycle_proposer(ctx)?;
    enforce_tee_lease_deadlines(ctx)?;
    Ok(())
}

/// Applies the canonical post-bootstrap TEE lease deadline to the bounded ACTIVE
/// validator set. The sweep is part of receipt-visible `ValidatorLiveness`, before any
/// user transaction in the block. Block 1 is excluded because its earlier
/// `TeeBootstrap` system transaction creates the founder bindings atomically.
pub(super) fn enforce_tee_lease_deadlines(ctx: &BlockRuntimeContext) -> Result<usize> {
    if ctx.block.block_number
        <= crate::tee_attestation_activation::TEE_ATTESTATION_V1_ACTIVATION_HEIGHT
    {
        return Ok(0);
    }

    let active = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone())
        .get_active_validators()?;
    let registry = outbe_teeregistry::TeeRegistry::new(ctx.storage.clone());
    let mut overdue = Vec::new();
    for validator in active {
        let binding = registry.validator_enclave_binding_v1(validator.validator_address)?;
        if binding.is_none_or(|binding| binding.valid_until <= ctx.block.timestamp) {
            overdue.push(validator.validator_address);
        }
    }
    if overdue.is_empty() {
        return Ok(0);
    }

    // Admit the percent and the clock before any jail. One checkpoint holds
    // every jail, slash, and log. A later failure rolls the whole sweep back.
    let transitioned = ctx.storage.with_checkpoint(|| {
        let percent = outbe_slashindicator::contract::SlashIndicator::new(ctx.storage.clone())
            .slash_amount_percent()?;
        ctx.storage.now_secs()?;
        let mut validator_set =
            outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
        let mut staking = outbe_staking::contract::Staking::new(ctx.storage.clone());
        let mut transitioned = Vec::new();
        for validator in overdue {
            if validator_set.jail_validator_for_tee_expiry(validator)? {
                staking.slash_stake(validator, percent)?;
                transitioned.push(validator);
            }
        }
        Ok(transitioned)
    })?;

    for validator in &transitioned {
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
            block_number = ctx.block.block_number,
            "validator jailed and slashed after TEE lease deadline",
        );
    }
    Ok(transitioned.len())
}

fn validate_and_record_cycle_proposer(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut vs = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
    if vs.is_consensus_participant(ctx.block.proposer)? {
        vs.record_proposer(ctx.block.proposer)?;
        Ok(())
    } else {
        Err(
            crate::errors::BeginBlockPrecompileError::ProposerNotConsensusParticipant {
                proposer: ctx.block.proposer,
            }
            .into(),
        )
    }
}

pub fn enforce_enclave_upgrade_deadline(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut registry = outbe_teeregistry::TeeRegistry::new(ctx.storage.clone());
    let Some(upgrade) = registry.upgrade_sweep_due_v1()? else {
        return Ok(());
    };
    let mut reports = Vec::new();
    ctx.with_checkpoint(|| {
        let slash_percent = outbe_slashindicator::schema::SlashIndicator::new(ctx.storage.clone())
            .slash_amount_percent()?;
        let mut validators = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
        let active = validators.get_active_validators()?;
        for validator in active {
            let address = validator.validator_address;
            let updated = registry
                .validator_enclave_binding_v1(address)?
                .is_some_and(|binding| {
                    binding.policy_hash == upgrade.successor_policy_hash
                        && binding.mrenclave == upgrade.mrenclave
                });
            if updated || registry.upgrade_penalty_applied_v1(upgrade.proposal_id, address)? {
                continue;
            }
            if let Some(report) = validators.jail_validator_deferred(address)? {
                reports.push(report);
            }
            let slashed = outbe_staking::contract::Staking::new(ctx.storage.clone())
                .slash_stake(address, slash_percent)?;
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
