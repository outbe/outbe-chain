use super::*;

/// Runs the Outbe pre-execution hook chain against a pre-built runtime context.
///
/// Exercised by `OutbeBlockExecutor::apply_pre_execution_changes` against a Reth
/// `StateDB` wrapped in `DirectStorageProvider`, and by lifecycle-level tests
/// against `HashMapStorageProvider`. Ordering is load-bearing:
///
/// 1. Genesis-state validation (blocks 0/1 only, if consensus config was supplied).
/// 2. `VoteLifecycle::begin_block` - tally expired proposals and dispatch approved ones.
/// 3. `UpdateLifecycle::begin_block_with_handlers` - activate scheduled updates at activation height.
/// 4. `RewardsLifecycle::begin_block` - locks in `genesis_utc_day` on
///    block 0; the per-block emission and per-day settle paths have
///    moved to the Cycle module.
/// 5. Metadosis WWD state machine has moved to the hourly ProtocolCycle
///    handler; no per-block hook here anymore.
/// 6. Staking matured-unbonding processing.
/// 7. `OracleLifecycle::begin_block` - tally + daily S-curve only.
///
/// Oracle slash-window force-exits run later as the receipt-visible
/// `OracleSlashWindow` begin-zone system phase, after optional `BoundaryOutcome`.
/// This preserves same-block boundary activation before any deterministic Oracle
/// penalty can mark a target validator EXITING while keeping operator-critical
/// Oracle events in normal EVM receipts.

pub fn run_outbe_pre_execution_hooks(
    hook_ctx: &BlockRuntimeContext,
    genesis_validators: Option<&GenesisValidators>,
) -> outbe_primitives::error::Result<()> {
    run_outbe_pre_execution_hooks_inner(hook_ctx, genesis_validators, None)
}

/// Runs pre-execution hooks with explicit off-chain body read authority.
pub fn run_outbe_pre_execution_hooks_with_readers(
    hook_ctx: &BlockRuntimeContext,
    genesis_validators: Option<&GenesisValidators>,
    readers: &RuntimeBodyReaders,
    scope: &ExecutionScope,
) -> outbe_primitives::error::Result<()> {
    run_outbe_pre_execution_hooks_inner(hook_ctx, genesis_validators, Some((readers, scope)))
}

fn run_outbe_pre_execution_hooks_inner(
    hook_ctx: &BlockRuntimeContext,
    genesis_validators: Option<&GenesisValidators>,
    readers: Option<(&RuntimeBodyReaders, &ExecutionScope)>,
) -> outbe_primitives::error::Result<()> {
    let block_number = hook_ctx.block.block_number;
    let timestamp = hook_ctx.block.timestamp;

    // Genesis state must be present in genesis.json. The executor only
    // verifies the local validators config against that canonical state.
    if block_number <= 1 {
        if let Some(genesis) = genesis_validators {
            validate_genesis_state(hook_ctx.storage.clone(), genesis)?;
        }
    }

    // Vote: tally expired proposals and dispatch approved ones.
    outbe_vote::lifecycle::VoteLifecycle::begin_block_with_handlers(
        hook_ctx,
        crate::handlers::vote::registry(),
    )?;

    // Update: activate scheduled updates at activation height.
    outbe_update::lifecycle::UpdateLifecycle::begin_block_with_handlers(
        hook_ctx,
        crate::handlers::update::registry(),
    )?;
    enforce_enclave_upgrade_deadline(hook_ctx)?;

    // EmissionLimit no longer participates in pre-execution lifecycle.
    // Per-block emission dispatch was removed (Phase 4 of
    // the Cycle epic) - the closed-form daily cap, sink allocation,
    // and AgentReward / Metadosis dispatch all run from ProtocolCycle's
    // persisted UTC-day decision instead.

    // Rewards lifecycle: locks in `genesis_utc_day` on block 0. Day-
    // boundary settle moved out of Rewards (Phase 3); the
    // Cycle handler now owns the daily orchestration.
    <outbe_rewards::lifecycle::RewardsLifecycle as BlockLifecycle>::begin_block(hook_ctx)?;

    // Metadosis WWD state machine + lysis distribution moved to the
    // Cycle handler. The
    // legacy `MetadosisLifecycle::begin_block` lifecycle hook used to
    // run here on every block; it is now invoked once per hourly
    // `outbe_cycle::handler::run_protocol_cycle` pass, after an optional
    // contiguous-day `dispatch_terminal_remainder_at` write.

    // Staking: process matured unbonding entries.
    outbe_staking::hooks::process_unbonding(hook_ctx.storage.clone(), timestamp)?;

    // Oracle: tally at vote period boundary and run daily S-curve. Slash-window
    // force-exits run later in the receipt-visible OracleSlashWindow system phase
    // so Phase 3 BoundaryOutcome can activate its target set before Oracle marks
    // underperformers EXITING.
    <outbe_oracle::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(hook_ctx)?;

    // Nod qualification and call/forfeit mutate compressed bucket bodies, so
    // the daily trigger and per-block continuation both run inside the
    // receipt-visible CycleTick system transaction (not here). Oracle has
    // already published the rate that transaction observes.
    let _ = readers;

    // GEM: carry on the daily qualify and call sweeps the Cycle trigger opened,
    // both pinned to a closed UTC day. Reads the same Oracle surface, so it must
    // run after Oracle.
    <outbe_gem::GemLifecycle as BlockLifecycle>::begin_block(hook_ctx)?;

    // INTEX: carry on the same two sweeps for series, plus the payout and expiry
    // drains. Reads the same Oracle surface, so it runs after Oracle.
    <outbe_intexfactory::IntexLifecycle as BlockLifecycle>::begin_block(hook_ctx)?;

    Ok(())
}

pub(in crate::executor) fn run_atomic_storage_hooks<DB, F>(
    db: &mut DB,
    ctx: BlockContext,
    hooks: F,
) -> Result<(AddressMap<Account>, Vec<Log>), BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    F: FnOnce(&BlockRuntimeContext) -> outbe_primitives::error::Result<()>,
{
    run_atomic_storage_hook_with_output(db, ctx, hooks)
        .map(|(changes, events, ())| (changes, events))
}

pub(in crate::executor) fn run_atomic_storage_hook_with_output<DB, F, R>(
    db: &mut DB,
    ctx: BlockContext,
    hooks: F,
) -> Result<(AddressMap<Account>, Vec<Log>, R), BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    F: FnOnce(&BlockRuntimeContext) -> outbe_primitives::error::Result<R>,
{
    let mut provider = DirectStorageProvider::new(db, ctx.clone());
    let storage = StorageHandle::new(&mut provider);
    let runtime_ctx = BlockRuntimeContext::new(ctx, storage.clone());

    let result = storage.with_checkpoint(|| hooks(&runtime_ctx));

    // Preserve the concrete hook error across the executor boundary. Payload
    // construction must distinguish node-local readiness (for example a stale
    // compressed-tree parent) from deterministic corruption; stringifying here
    // destroys that distinction and turns a cancellable job into an alarm.
    let output = result.map_err(BlockExecutionError::other)?;

    provider.flush().map_err(|e| {
        BlockExecutionError::Internal(InternalBlockExecutionError::Other(
            format!("outbe hook flush: {e}").into(),
        ))
    })?;

    let changes = provider.take_committed_changes();
    let events = provider.take_events();
    Ok((changes, events, output))
}

pub(crate) fn enforce_enclave_upgrade_deadline(
    ctx: &BlockRuntimeContext,
) -> outbe_primitives::error::Result<()> {
    let mut registry = outbe_teeregistry::TeeRegistry::new(ctx.storage.clone());
    let Some(upgrade) = registry.upgrade_sweep_due_v1()? else {
        return Ok(());
    };
    let mut reports = Vec::new();
    ctx.with_checkpoint(|| {
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
            // Jail first: the stake reducer must preserve the jailed lifecycle.
            if let Some(report) = validators.jail_validator_deferred(address)? {
                reports.push(report);
            }
            let slashed = outbe_staking::contract::Staking::new(ctx.storage.clone())
                .slash_stake(address, 10)?;
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
