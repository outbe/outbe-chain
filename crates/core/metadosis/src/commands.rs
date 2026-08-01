use alloy_primitives::{Bytes, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    storage::{
        metadosis_advance_due_binding, metadosis_init_genesis_binding,
        metadosis_ocomp_lifecycle_begin_binding, metadosis_ocomp_terminal_request_binding,
        metadosis_process_ready_binding, metadosis_verified_vote_binding,
        MetadosisCertifiedFinality, MetadosisCertifiedFinalityBinding, MetadosisCycleLifecycle,
        MetadosisForkProfile, MetadosisOcompLifecycle, MetadosisVerifiedResultVote, StorageHandle,
    },
};

use crate::{commit::commit_transition, schema::MetadosisContract};

pub fn init_genesis_day(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let binding = metadosis_init_genesis_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    commit_transition::<MetadosisCycleLifecycle, _>(ctx.storage.clone(), binding, |_| {
        crate::runtime::init_genesis_day(ctx)
    })
}

pub fn start_metadosis(
    ctx: &BlockRuntimeContext<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    let binding = metadosis_process_ready_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    with_ce_checkpoint(scope, || {
        commit_transition::<MetadosisCycleLifecycle, _>(ctx.storage.clone(), binding, |_| {
            crate::runtime::start_metadosis(ctx, scope, parent)
        })
    })
}

/// Purpose-bound terminal-sink adapter used by the Cycle-owned daily
/// allocation flow. The internal sink returns a typed semantic receipt; after
/// exhaustive confirmation that the limit is formed, the adapter returns zero
/// because the allocation has no unused amount.
pub fn apply_cycle_day_limit(ctx: &BlockRuntimeContext<'_>, amount: U256) -> Result<U256> {
    match crate::emission_sink::apply(ctx, amount)? {
        crate::state::DayLimitFormationReceipt::Formed(_) => Ok(U256::ZERO),
    }
}

/// Purpose-bound late-settlement adapter. Late residue is carry-over only and
/// cannot form or replace a Cycle-owned base day limit.
pub fn apply_late_settlement_headroom(ctx: &BlockRuntimeContext<'_>, amount: U256) -> Result<U256> {
    crate::emission_sink::apply_late_settlement_headroom(ctx, amount)
}

pub fn advance_active_worldwide_days(
    ctx: &BlockRuntimeContext<'_>,
    scope: &ExecutionScope,
) -> Result<()> {
    let binding = metadosis_advance_due_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    with_ce_checkpoint(scope, || {
        commit_transition::<MetadosisCycleLifecycle, _>(ctx.storage.clone(), binding, |_| {
            crate::runtime::advance_active_worldwide_days(ctx, scope)
        })
    })
}

fn with_ce_checkpoint<R>(scope: &ExecutionScope, effect: impl FnOnce() -> Result<R>) -> Result<R> {
    let checkpoint = scope.ce_work_checkpoint()?;
    match effect() {
        Ok(result) => Ok(result),
        Err(error) => {
            scope.restore_ce_work_checkpoint(checkpoint)?;
            Err(error)
        }
    }
}

pub fn record_certified_parent_finality(
    ctx: &BlockRuntimeContext<'_>,
    certified: &MetadosisCertifiedFinalityBinding,
) -> Result<bool> {
    if certified.chain_id() != ctx.block.chain_id
        || certified.current_block_number() != ctx.block.block_number
    {
        return Err(outbe_primitives::error::PrecompileError::Fatal(
            "certified Metadosis finality binding execution scope mismatch".into(),
        ));
    }
    commit_transition::<MetadosisCertifiedFinality, _>(
        ctx.storage.clone(),
        certified.binding(),
        |_| {
            crate::ocomp::expiry::record_certified_parent_finality(
                ctx,
                certified.finalized_block_number(),
                certified.finalized_block_hash(),
                certified.finalized_state_root(),
            )
        },
    )
}

pub fn install_fork_profile(
    ctx: &BlockRuntimeContext<'_>,
    install: &crate::config::OcompForkInstallV1,
) -> Result<()> {
    let limits = crate::config::poc_schema_limits();
    let binding = install.install_hash(&limits)?;
    commit_transition::<MetadosisForkProfile, _>(ctx.storage.clone(), binding, |storage| {
        MetadosisContract::new(storage).initialize_ocomp_fork_install(
            install,
            ctx.block.block_number,
            &limits,
        )
    })
}

pub fn run_ocomp_lifecycle_begin(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let binding = metadosis_ocomp_lifecycle_begin_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    commit_transition::<MetadosisOcompLifecycle, _>(ctx.storage.clone(), binding, |_| {
        crate::ocomp::expiry::run_lifecycle_begin(ctx)
    })
}

pub fn run_ocomp_terminal_request(
    ctx: &BlockRuntimeContext<'_>,
    scope: &ExecutionScope,
) -> Result<()> {
    let binding = metadosis_ocomp_terminal_request_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    commit_transition::<MetadosisOcompLifecycle, _>(ctx.storage.clone(), binding, |_| {
        crate::ocomp::request::run_terminal_request(ctx, scope)
    })
}

pub fn submit_verified_result_vote(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    data: &[u8],
    value: U256,
    is_static: bool,
) -> Result<Bytes> {
    let binding = metadosis_verified_vote_binding(data);
    with_ce_checkpoint(scope, || {
        commit_transition::<MetadosisVerifiedResultVote, _>(storage.clone(), binding, |_| {
            crate::ocomp::vote::dispatch_public_result_vote(storage, scope, data, value, is_static)
        })
    })
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, U256};
    use outbe_common::WorldwideDay;
    use outbe_compressed_entities::{begin_block, mint, BodyInput, EntityId36, TributeBodyV1};
    use outbe_primitives::{
        addresses::COMPRESSED_ENTITIES_ADDRESS,
        block::{BlockContext, BlockRuntimeContext},
        storage::{
            hashmap::HashMapStorageProvider, MetadosisCycleLifecycle, MetadosisMutationPurposeTag,
            StorageHandle,
        },
    };

    use super::*;

    #[test]
    fn fork_install_command_rolls_back_every_mutation_and_retries_exactly() {
        const CHAIN_ID: u64 = 1;
        const ACTIVATION_HEIGHT: u64 = crate::config::OCOMP_POC_FINAL_ACTIVATION_HEIGHT;
        let install = crate::test_support::ForkInstallScenario::final_at(
            ACTIVATION_HEIGHT,
            CHAIN_ID,
            B256::repeat_byte(0x91),
        )
        .unwrap()
        .into_install();

        let run = |provider: &mut HashMapStorageProvider| {
            provider.set_block_number(ACTIVATION_HEIGHT);
            provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::ForkProfile);
            StorageHandle::enter(provider, |storage| {
                let ctx = BlockRuntimeContext::new(
                    BlockContext::empty_for_tests(ACTIVATION_HEIGHT, 0, CHAIN_ID),
                    storage,
                );
                install_fork_profile(&ctx, &install)
            })
        };

        let mut control = HashMapStorageProvider::new(CHAIN_ID);
        control.fail_after_mutation_at(usize::MAX);
        run(&mut control).unwrap();
        let mutation_count = control.clear_mutation_failure();
        assert!(
            mutation_count >= 2,
            "fork install must persist both request and activation authorities"
        );
        let clean_storage = control.storage.clone();
        let clean_events = control.get_ordered_events().to_vec();

        for operation in 0..mutation_count {
            let mut provider = HashMapStorageProvider::new(CHAIN_ID);
            let storage_before = provider.storage.clone();
            let events_before = provider.get_ordered_events().to_vec();
            provider.fail_after_mutation_at(operation);

            assert!(matches!(
                run(&mut provider),
                Err(outbe_primitives::error::PrecompileError::Storage(_))
            ));
            assert_eq!(provider.clear_mutation_failure(), operation + 1);
            assert_eq!(provider.storage, storage_before, "storage at {operation}");
            assert_eq!(
                provider.get_ordered_events(),
                events_before.as_slice(),
                "ordered events at {operation}"
            );

            run(&mut provider).unwrap();
            assert_eq!(
                provider.storage, clean_storage,
                "fork install retry storage diverged at {operation}"
            );
            assert_eq!(
                provider.get_ordered_events(),
                clean_events.as_slice(),
                "fork install retry events diverged at {operation}"
            );
        }
    }

    #[test]
    fn command_ce_checkpoint_covers_post_effect_aggregate_validation() {
        let mut provider = HashMapStorageProvider::new(1);
        let scope = ExecutionScope::new();
        StorageHandle::enter(&mut provider, |storage| {
            storage
                .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(3))
                .unwrap();
            storage
                .sstore(
                    COMPRESSED_ENTITIES_ADDRESS,
                    U256::from(1),
                    U256::from_be_slice(
                        outbe_compressed_entities::sealed_root(B256::ZERO)
                            .unwrap()
                            .as_slice(),
                    ),
                )
                .unwrap();
            begin_block(storage, &scope).unwrap();
        });
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        let ce_before = scope.ce_work_checkpoint().unwrap();
        provider.set_block_number(1);
        provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);

        let wwd = WorldwideDay::new(2026_0817);
        let result = StorageHandle::enter(&mut provider, |storage| {
            with_ce_checkpoint(&scope, || {
                commit_transition::<MetadosisCycleLifecycle, _>(
                    storage,
                    B256::repeat_byte(0x83),
                    |effect_storage| {
                        mint(
                            effect_storage.clone(),
                            &scope,
                            BodyInput::Tribute(&TributeBodyV1 {
                                tribute_id: EntityId36::new(wwd, [0x84; 32]),
                                owner: Address::repeat_byte(0x85),
                                worldwide_day: wwd,
                                issuance_amount_minor: U256::from(1),
                                issuance_currency: 840,
                                nominal_amount_minor: U256::from(1),
                                reference_currency: 840,
                                tribute_price_minor: U256::from(1),
                                exclude_from_intex_issuance: false,
                            }),
                        )?;
                        crate::fixture_kernel::FixtureKernelExt::add_active_wwd(
                            &mut MetadosisContract::new(effect_storage),
                            wwd,
                        )
                    },
                )
            })
        });

        assert!(
            result.is_err(),
            "post-effect validation must reject active membership without a record"
        );
        assert_eq!(provider.storage, storage_before);
        assert_eq!(provider.events, events_before);
        assert_eq!(scope.ce_work_checkpoint().unwrap(), ce_before);
    }
}
