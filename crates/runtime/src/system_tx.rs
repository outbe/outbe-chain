//! Declarative dispatch of the twenty system-transaction kinds.
//!
//! The match is composition only. Each arm calls the domain entrypoint.

use alloy_primitives::{Address, Bytes, U256};
use outbe_evm::errors::BeginBlockPrecompileError;
use outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1;
use outbe_primitives::addresses::SYSTEM_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext, ProtocolModule};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::system_tx::SystemTx;
use outbe_primitives::system_tx_context::SystemTxExecutionContext;
use outbe_primitives::tee_attestation_v1::TeePolicyV1;
use outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2;

/// Selectors on the system-transaction precompile that accept native value.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

/// Route-table entry. The executor path passes the ChainSpec TEE authority
/// through [`dispatch`]; this entry uses the unbound authority.
pub fn dispatch_route(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    dispatch(
        storage,
        data,
        caller,
        value,
        &TeeAttestationChainSpecStateV1::Unbound,
    )
}

/// Dispatches one decoded system transaction.
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
    tee: &TeeAttestationChainSpecStateV1,
) -> Result<Bytes> {
    if caller != SYSTEM_ADDRESS {
        return Err(BeginBlockPrecompileError::InvalidSystemCaller.into());
    }
    if !value.is_zero() {
        return Err(BeginBlockPrecompileError::NativeValueRejected.into());
    }
    outbe_primitives::dispatch::ensure_mutation_allowed(&storage)?;

    #[cfg(feature = "test-hooks")]
    if let Some((dirty_addr, dirty_slot, dirty_val)) =
        outbe_evm::sys_tx_precompile::forced_dirty_system_precompile_revert()
    {
        storage.sstore(dirty_addr, dirty_slot, dirty_val)?;
        storage.emit_event(
            dirty_addr,
            alloy_primitives::LogData::new_unchecked(
                vec![alloy_primitives::keccak256("DirtySoftFailEvent()")],
                Bytes::from_static(b"dirty-soft-fail-log"),
            ),
        )?;
        return Err(outbe_primitives::error::PrecompileError::Revert(
            "forced dirty system precompile revert".into(),
        ));
    }

    let input = SystemTx::decode(data).map_err(|error| {
        PrecompileError::from(BeginBlockPrecompileError::InvalidSystemTxInput(
            error.to_string(),
        ))
    })?;

    match input {
        SystemTx::CertifiedParentAccounting { metadata } => {
            let ctx = block_runtime_context(storage, true)?;
            let execution = outbe_evm::sys_tx_precompile::current_preloaded_system_tx_context()
                .unwrap_or_else(|| {
                    SystemTxExecutionContext::for_end_block(
                        ctx.block.proposer,
                        ctx.block.block_gas_limit,
                        ctx.block.user_gas_used,
                    )
                });
            outbe_rewards::finalization::run_finalization_and_slashing(
                &ctx, &metadata, &execution,
            )?;
        }
        SystemTx::LateFinalizeCredits { artifact } => {
            let ctx = block_runtime_context(storage, false)?;
            let execution = outbe_evm::sys_tx_precompile::current_preloaded_system_tx_context();
            outbe_rewards::finalization::run_late_finalize_credits(
                &ctx,
                &artifact,
                execution.as_ref(),
            )?;
        }
        SystemTx::BoundaryOutcome { artifact } => {
            let ctx = block_runtime_context(storage, true)?;
            outbe_validatorset::boundary::apply_boundary_outcome(
                ctx.storage.clone(),
                &artifact,
                ctx.block.block_number,
                ctx.block.timestamp,
            )?;
            outbe_hyperlanecontroller::lifecycle::sync_validators(&ctx)?;
        }
        SystemTx::TeeBootstrap { payload } => {
            let ctx = block_runtime_context(storage, true)?;
            execute_tee_bootstrap(&ctx, &payload, tee)?;
        }
        SystemTx::ValidatorLiveness => {
            let ctx = block_runtime_context(storage, true)?;
            outbe_slashindicator::run_validator_liveness(&ctx)?;
        }
        SystemTx::RadiclePublication { batch } => {
            outbe_coderegistry::CodeRegistry::new(storage).apply_publication_batch(&batch)?;
        }
        SystemTx::Vote => {
            let ctx = block_runtime_context(storage, true)?;
            outbe_vote::VoteModule::begin_block(&ctx)?;
        }
        SystemTx::Update => {
            let ctx = block_runtime_context(storage, false)?;
            outbe_update::UpdateModule::begin_block(&ctx)?;
        }
        SystemTx::EnclaveUpgradeDeadline => {
            let ctx = block_runtime_context(storage, false)?;
            outbe_slashindicator::enforce_enclave_upgrade_deadline(&ctx)?;
        }
        SystemTx::Rewards => {
            let ctx = block_runtime_context(storage, false)?;
            outbe_rewards::RewardsModule::begin_block(&ctx)?;
        }
        SystemTx::Staking => {
            let ctx = block_runtime_context(storage, false)?;
            outbe_staking::StakingModule::begin_block(&ctx)?;
        }
        SystemTx::Oracle => {
            let ctx = block_runtime_context(storage, true)?;
            outbe_oracle::OracleModule::begin_block(&ctx)?;
        }
        SystemTx::OracleSlash => {
            let ctx = block_runtime_context(storage, false)?;
            outbe_oracle::OracleSlashModule::begin_block(&ctx)?;
        }
        SystemTx::HyperlaneSlash => {
            let ctx = block_runtime_context(storage, false)?;
            outbe_hyperlanecontroller::HyperlaneModule::begin_block(&ctx)?;
        }
        SystemTx::CycleTick => {
            let ctx = block_runtime_context(storage, true)?;
            outbe_cycle::CycleModule::begin_block(&ctx)?;
        }
        SystemTx::NodFactory => {
            let ctx = block_runtime_context(storage, false)?;
            outbe_nodfactory::NodFactoryModule::end_block(&ctx)?;
        }
        SystemTx::GemFactory => {
            let ctx = block_runtime_context(storage, false)?;
            outbe_gemfactory::GemFactoryModule::end_block(&ctx)?;
        }
        SystemTx::IntexFactory => {
            let ctx = block_runtime_context(storage, false)?;
            outbe_intexfactory::IntexFactoryModule::end_block(&ctx)?;
        }
        SystemTx::CredisFactory => {
            let ctx = block_runtime_context(storage, false)?;
            outbe_credisfactory::CredisFactoryModule::end_block(&ctx)?;
        }
        SystemTx::GemCleanup => {
            let ctx = block_runtime_context(storage, false)?;
            outbe_gemfactory::GemCleanupModule::end_block(&ctx)?;
        }
    }
    Ok(Bytes::new())
}

fn block_runtime_context(
    storage: StorageHandle,
    include_validator_snapshot: bool,
) -> Result<BlockRuntimeContext> {
    let block_number = storage.block_number()?;
    let timestamp = storage.now_secs()?;
    let chain_id = storage.chain_id()?;
    let genesis_hash = storage.genesis_hash()?;
    let preloaded = outbe_evm::sys_tx_precompile::current_preloaded_system_tx_context();
    let proposer = preloaded
        .as_ref()
        .map(|context| context.proposer)
        .unwrap_or(storage.beneficiary()?);
    let block_gas_limit = preloaded
        .as_ref()
        .map(|context| context.block_gas_limit)
        .unwrap_or(0);
    let user_gas_used = preloaded
        .as_ref()
        .map(|context| context.user_gas_used)
        .unwrap_or(0);
    let validators = if include_validator_snapshot {
        let mut validators: Vec<Address> =
            outbe_validatorset::contract::ValidatorSet::new(storage.clone())
                .get_active_consensus_set()?
                .into_iter()
                .map(|record| record.validator_address)
                .collect();
        validators.sort();
        validators
    } else {
        Vec::new()
    };
    let block = BlockContext {
        block_gas_limit,
        user_gas_used,
        block_number,
        timestamp,
        chain_id,
        genesis_hash,
        genesis_timestamp: storage.genesis_timestamp()?,
        proposer,
        validators,
    };
    #[cfg(feature = "test-hooks")]
    outbe_evm::sys_tx_precompile::record_observed_runtime_block_context(&block);
    Ok(BlockRuntimeContext::new(block, storage))
}

fn execute_tee_bootstrap(
    ctx: &BlockRuntimeContext<'_>,
    payload: &TeeBootstrapV2,
    state: &TeeAttestationChainSpecStateV1,
) -> Result<()> {
    let expected_policy = resolve_expected_policy(ctx.block.block_number, payload, state)?;
    outbe_teeregistry::TeeRegistry::new(ctx.storage.clone())
        .install_initial_policy_v1(&expected_policy)?;
    outbe_teeregistry::run_tee_bootstrap_v1(ctx, payload, &expected_policy)
}

fn resolve_expected_policy(
    block_number: u64,
    payload: &TeeBootstrapV2,
    state: &TeeAttestationChainSpecStateV1,
) -> Result<TeePolicyV1> {
    let activation = state.activation().map_err(|error| {
        PrecompileError::from(BeginBlockPrecompileError::InvalidTeeChainSpecAuthority(
            error.to_string(),
        ))
    })?;
    if block_number < activation.manifest.activation_height {
        return Err(BeginBlockPrecompileError::Ost3NotActive.into());
    }
    let expected_policy = activation.policy_at(block_number).map_err(|error| {
        PrecompileError::from(BeginBlockPrecompileError::InvalidActiveTeePolicy(
            error.to_string(),
        ))
    })?;
    if &payload.authority.policy != expected_policy {
        return Err(BeginBlockPrecompileError::Ost3PolicyScheduleMismatch.into());
    }
    Ok(expected_policy.clone())
}
