//! Runtime-owned protocol order, dispatch, and system-call authority.

use alloy_primitives::{Address, Bytes, U256};
use outbe_evm::errors::BeginBlockPrecompileError;
use outbe_evm::precompiles::MetadosisMutationCall;
use outbe_primitives::addresses::{OUTBE_SYSTEM_TX_ADDRESS, SYSTEM_ADDRESS};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::reshare_artifact::{
    decode_boundary_artifact, encode_boundary_artifact, ConsensusHeaderArtifact,
};
use outbe_primitives::storage::{MetadosisMutationEntitlements, StorageHandle};
use outbe_primitives::system_tx::{
    binding::validate_boundary_outcome_payload, SystemTx, CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
    CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX, CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
    CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX, CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
    CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX, MAX_END_BLOCK_SYSTEM_TXS, MAX_PREFIX_SYSTEM_TXS,
};
use outbe_primitives::system_tx_context::{
    SystemTxBuildContext, SystemTxContext, SystemTxPayloadBuildError,
    SystemTxPayloadValidationError, SystemTxValidationContext,
};
use outbe_primitives::tee_attestation_v1::TeePolicyV1;
use outbe_tee::tee_attestation_activation::TeeAttestationChainSpecStateV1;
use {
    outbe_evm::system_tx::{begin_step, consensus_step, end_step, BlockOrder, ProtocolStep},
    outbe_primitives::block::{
        BlockContext, BlockRuntimeContext, MetadosisEntitlementContext, ProtocolModule,
    },
};

/// Applies the certified validator boundary before begin-block work.
pub struct BoundaryOutcomeModule;

impl ProtocolModule for BoundaryOutcomeModule {
    const NAME: &'static str = "boundary_outcome";

    fn build_payload(
        ctx: &SystemTxBuildContext<'_>,
    ) -> std::result::Result<Bytes, SystemTxPayloadBuildError> {
        let Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) =
            ctx.artifacts.consensus_header_artifact.as_ref()
        else {
            return Err(SystemTxPayloadBuildError::MissingBlock1BoundaryOutcome);
        };
        Ok(encode_boundary_artifact(artifact)?)
    }

    fn validate_payload(
        payload: &[u8],
        ctx: &SystemTxValidationContext<'_>,
    ) -> std::result::Result<(), SystemTxPayloadValidationError> {
        validate_boundary_outcome_payload(payload, ctx.artifacts)?;
        Ok(())
    }

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        let sys_ctx = ctx.system_tx()?;
        let artifact = decode_boundary_artifact(sys_ctx.payload.as_ref())?.ok_or_else(|| {
            BeginBlockPrecompileError::InvalidSystemTxInput(
                "missing boundary outcome payload".into(),
            )
        })?;
        if encode_boundary_artifact(&artifact)?.as_ref() != sys_ctx.payload.as_ref() {
            return Err(BeginBlockPrecompileError::InvalidSystemTxInput(
                "non-canonical boundary outcome payload".into(),
            )
            .into());
        }
        outbe_validatorset::boundary::apply_boundary_outcome(
            ctx.storage.clone(),
            &artifact,
            ctx.block.block_number,
            ctx.block.timestamp,
        )?;
        outbe_hyperlanecontroller::lifecycle::sync_validators(ctx)
    }
}

pub const CONSENSUS_ORDER: &[ProtocolStep] = &[
    consensus_step::<outbe_rewards::finalization::ParentAccountingModule>()
        .with_validators()
        .active_when(|regime, _| regime.has_parent_finalization_accounting()),
    consensus_step::<outbe_rewards::finalization::LateFinalizeCreditsModule>()
        .active_when(|regime, _| regime.has_parent_finalization_accounting()),
    consensus_step::<BoundaryOutcomeModule>()
        .with_validators()
        .active_when(|regime, flags| {
            regime.requires_genesis_bootstrap_txs() || flags.has_boundary_outcome
        }),
    consensus_step::<outbe_teeregistry::TeeBootstrapModule>()
        .with_validators()
        .active_when(|regime, _| regime.requires_genesis_bootstrap_txs()),
    consensus_step::<outbe_slashindicator::ValidatorLivenessModule>()
        .with_validators()
        .active_when(|regime, _| regime.has_system_tx_prefix()),
    consensus_step::<outbe_coderegistry::RadiclePublicationModule>()
        .active_when(|_, flags| flags.has_publication),
];

pub const BEGIN_BLOCK_ORDER: &[ProtocolStep] = &[
    begin_step::<outbe_vote::VoteModule>().with_validators(),
    begin_step::<outbe_update::UpdateModule>(),
    begin_step::<outbe_slashindicator::EnclaveUpgradeDeadline>(),
    begin_step::<outbe_rewards::RewardsModule>(),
    begin_step::<outbe_staking::StakingModule>(),
    begin_step::<outbe_oracle::OracleModule>().with_validators(),
    begin_step::<outbe_oracle::OracleSlashModule>().soft_fail(),
    begin_step::<outbe_hyperlanecontroller::HyperlaneModule>().soft_fail(),
    begin_step::<outbe_cycle::CycleModule>().with_validators(),
];

pub const END_BLOCK_ORDER: &[ProtocolStep] = &[
    end_step::<outbe_nodfactory::NodFactoryModule>().with_process_ready_entitlements(),
    end_step::<outbe_gemfactory::GemFactoryModule>().with_process_ready_entitlements(),
    end_step::<outbe_intexfactory::IntexFactoryModule>().with_process_ready_entitlements(),
    end_step::<outbe_credisfactory::CredisFactoryModule>().with_process_ready_entitlements(),
    end_step::<outbe_gemfactory::GemCleanupModule>().with_process_ready_entitlements(),
];

pub const BLOCK_ORDER: BlockOrder = BlockOrder {
    consensus: CONSENSUS_ORDER,
    begin_block: BEGIN_BLOCK_ORDER,
    end_block: END_BLOCK_ORDER,
};

const _: () = {
    assert!(CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX == 0);
    assert!(CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX == 1);
    assert!(CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX == 2);
    assert!(CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX == 3);
    assert!(CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX == 4);
    assert!(CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX == 5);
    assert!(CONSENSUS_ORDER.len() == 6);
    assert!(BEGIN_BLOCK_ORDER.len() <= MAX_PREFIX_SYSTEM_TXS - CONSENSUS_ORDER.len());
    assert!(END_BLOCK_ORDER.len() <= MAX_END_BLOCK_SYSTEM_TXS);
};

fn resolve_system_tx_step(data: &[u8]) -> Option<(&'static ProtocolStep, Bytes)> {
    let input = SystemTx::decode(data).ok()?;
    Some((
        BLOCK_ORDER.step_for_tx(&input)?,
        Bytes::copy_from_slice(input.payload()),
    ))
}

pub fn is_protocol_cycle_call(address: Address, caller: Address, data: &[u8]) -> bool {
    address == OUTBE_SYSTEM_TX_ADDRESS
        && caller == SYSTEM_ADDRESS
        && resolve_system_tx_step(data).is_some_and(|(step, _)| step.needs_cycle_active_utc_day)
}

pub fn metadosis_mutation_entitlements(
    call: MetadosisMutationCall<'_>,
) -> MetadosisMutationEntitlements {
    if call.is_static
        || !call.value.is_zero()
        || call.address != OUTBE_SYSTEM_TX_ADDRESS
        || call.caller != SYSTEM_ADDRESS
    {
        return MetadosisMutationEntitlements::NONE;
    }
    let Some((step, payload)) = resolve_system_tx_step(call.data) else {
        return MetadosisMutationEntitlements::NONE;
    };
    (step.mutation_entitlements)(MetadosisEntitlementContext {
        payload: payload.as_ref(),
        chain_id: call.chain_id,
        block_number: call.block_number,
        timestamp: call.timestamp,
        cycle_active_utc_day: call.cycle_active_utc_day,
    })
}

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
        outbe_evm::executor::forced_dirty_system_precompile_revert()
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

    let step = BLOCK_ORDER.step_for_tx(&input).ok_or_else(|| {
        BeginBlockPrecompileError::InvalidSystemTxInput(format!(
            "{} step index {} out of bounds",
            input.stage_name(),
            input.step_index()
        ))
    })?;
    let chain_spec_tee_policy = if matches!(
        input,
        SystemTx::Consensus {
            step_index: CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
            ..
        }
    ) {
        Some(resolve_expected_policy(&storage, tee)?)
    } else {
        None
    };
    let sys_ctx = SystemTxContext::from_preloaded(
        Bytes::copy_from_slice(input.payload()),
        outbe_evm::executor::current_preloaded_system_tx_context(),
        chain_spec_tee_policy,
    );
    if matches!(
        input,
        SystemTx::Consensus {
            step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
            ..
        }
    ) && sys_ctx.finalized_summary.is_none()
    {
        let metadata = CertifiedParentAccountingMetadata::decode(sys_ctx.payload.as_ref())?;
        return Err(
            outbe_rewards::errors::FinalizationError::MissingPreloadedExecutionSummary {
                block_number: metadata.finalized_block_number,
                block_hash: metadata.finalized_block_hash,
            }
            .into(),
        );
    }
    let ctx = block_runtime_context(storage, step.include_validator_snapshot, Some(sys_ctx))?;
    (step.run)(&ctx)?;
    Ok(Bytes::new())
}

fn block_runtime_context(
    storage: StorageHandle,
    include_validator_snapshot: bool,
    system_tx: Option<SystemTxContext>,
) -> Result<BlockRuntimeContext> {
    let block_number = storage.block_number()?;
    let timestamp = storage.now_secs()?;
    let chain_id = storage.chain_id()?;
    let genesis_hash = storage.genesis_hash()?;
    let preloaded = outbe_evm::executor::current_preloaded_system_tx_context();
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
        crate::wiring::sorted_active_consensus_validators(storage.clone())?
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
    outbe_evm::executor::record_observed_runtime_block_context(&block);
    Ok(BlockRuntimeContext {
        block,
        storage,
        system_tx,
    })
}

fn resolve_expected_policy(
    storage: &StorageHandle<'_>,
    state: &TeeAttestationChainSpecStateV1,
) -> Result<TeePolicyV1> {
    let block_number = storage.block_number()?;
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
    Ok(expected_policy.clone())
}
