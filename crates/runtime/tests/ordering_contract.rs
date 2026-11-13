//! - executor ordering contract.
//!
//! Pins the invariant that backs the epoch-lag admissibility of the
//! slashindicator precompile. `ValidatorSet.epoch_number` names the committee
//! that a certified `BoundaryOutcome` actually activated. It does not name the
//! epoch whose nominal height was merely reached.
//!
//! The behaviour test here drives the begin-block modules against a primed
//! in-memory storage provider. It asserts that the activated epoch stays
//! untouched when a block reaches the nominal boundary without a certified
//! `BoundaryOutcome`. The receipt-visible BoundaryOutcome path owns
//! the later atomic epoch/member/snapshot switch before user transactions.

use alloy_primitives::{Address, U256};
use outbe_evm::system_tx::SystemTx;
use outbe_evm::EvmRuntimeHost;
use outbe_primitives::addresses::SYSTEM_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::test_support::StorageOverrides;
use outbe_validatorset::EpochSnapshot;

const CHAIN_ID: u64 = 1;
const EPOCH_LENGTH: u32 = 10;
const PROPOSER: Address = Address::ZERO;

/// Seeds the minimum on-chain state the pre-exec hook chain needs to reach a
/// nominal epoch boundary:
///   * `config_epoch_length_blocks = EPOCH_LENGTH`
///   * `epoch_start_block = 0`
///   * `epoch_number = 1` (the currently activated epoch)
fn seed_validator_set(storage: StorageHandle, initial_epoch: u64) {
    let mut vs = ValidatorSet::new(storage.clone());
    vs.test_set_epoch_snapshot(EpochSnapshot {
        number: U256::from(initial_epoch),
        start_timestamp: 0,
        start_block: 0,
        length_blocks: EPOCH_LENGTH,
    })
    .unwrap();
    // Seed COEN/840 pair + 1.0 rate so begin-block NOD/GEM/INTEX promotion
    // reads a registered pair instead of reverting "pair not registered".
    outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR).unwrap();
    outbe_oracle::api::set_exchange_rate(
        storage,
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        outbe_oracle::api::RateObservation {
            rate: U256::from(1_000_000u64),
            block_number: 0,
            timestamp: 0,
        },
    )
    .unwrap();
}

/// A block with no certified `BoundaryOutcome` does not reset epoch counters.
/// Begin-block modules still run and must leave the activated epoch in place.
fn run_begin_block_without_certified_boundary(ctx: &BlockRuntimeContext) {
    for (index, _) in outbe_runtime::BLOCK_ORDER.begin_block.iter().enumerate() {
        let kind = SystemTx::BeginBlock {
            step_index: u8::try_from(index).unwrap(),
        };
        let input = kind.encode().expect("begin-block system tx encodes");
        outbe_runtime::OutbeRuntime::new()
            .dispatch_system_tx(
                ctx.storage.clone(),
                &input,
                SYSTEM_ADDRESS,
                U256::ZERO,
                &outbe_tee::tee_attestation_activation::TeeAttestationChainSpecStateV1::Unbound,
            )
            .expect("begin-block module must succeed");
    }
}

fn arm_begin_block_gas(provider: &mut HashMapStorageProvider) {
    provider.enable_production_storage_gas_metering();
    provider.set_gas_limit(SYSTEM_TX_ARTIFACT_GAS_LIMIT);
}

#[test]
fn nominal_epoch_boundary_without_certified_outcome_keeps_activated_epoch() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let boundary_block = EPOCH_LENGTH as u64;
    provider.set_block_number(boundary_block);
    arm_begin_block_gas(&mut provider);

    provider.enter(|storage| {
        // (1) Seed the boundary-crossing epoch state.
        seed_validator_set(storage.clone(), 1);

        // Sanity: epoch_number BEFORE the pre-exec hook chain is the
        // pre-bump value.
        let vs_before = ValidatorSet::new(storage.clone());
        let epoch_before = vs_before.epoch_snapshot().unwrap();
        assert_eq!(
            epoch_before.number,
            U256::from(1u64),
            "pre-condition: epoch_number must be 1 before pre-exec",
        );
        assert_eq!(
            epoch_before.start_block, 0,
            "pre-condition: epoch_start_block must be 0 before pre-exec",
        );

        // (2) Drive begin-block with no certified BoundaryOutcome.
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: boundary_block,
                timestamp: /*timestamp=*/ 1_700_000_000,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 1_700_000_000,
                proposer: PROPOSER,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        run_begin_block_without_certified_boundary(&ctx);

        // (3) The scheduled height is not activation authority. Until the
        // receipt-visible BoundaryOutcome executes, every consumer - including
        // native Lysis - must continue to observe the old epoch and its snapshot.
        let vs_after = ValidatorSet::new(storage);
        let epoch_after = vs_after.epoch_snapshot().unwrap();
        assert_eq!(
            epoch_after.number,
            U256::from(1u64),
            "nominal boundary without certified outcome must not advance epoch",
        );
        assert_eq!(
            epoch_after.start_block, 0,
            "nominal boundary without certified outcome must not move epoch anchor",
        );
    });
}

/// Companion negative test: an ordinary mid-epoch block also leaves the
/// activated epoch untouched.
#[test]
fn transition_epoch_does_not_fire_inside_an_epoch() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let mid_epoch_block = (EPOCH_LENGTH as u64) / 2;
    provider.set_block_number(mid_epoch_block);
    arm_begin_block_gas(&mut provider);

    provider.enter(|storage| {
        seed_validator_set(storage.clone(), 1);

        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: mid_epoch_block,
                timestamp: 1_700_000_000,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 1_700_000_000,
                proposer: PROPOSER,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        run_begin_block_without_certified_boundary(&ctx);

        let vs_after = ValidatorSet::new(storage);
        let epoch_after = vs_after.epoch_snapshot().unwrap();
        assert_eq!(
            epoch_after.number,
            U256::from(1u64),
            "mid-epoch block must NOT bump epoch_number",
        );
        assert_eq!(
            epoch_after.start_block, 0,
            "mid-epoch block must NOT advance epoch_start_block",
        );
    });
}

#[test]
fn runtime_order_matches_protocol_contract_and_lower_layer_order() {
    use {outbe_evm::system_tx::TEST_BLOCK_ORDER, outbe_primitives::block::BlockHeightRegime};
    let policy = <outbe_runtime::OutbeRuntime as outbe_consensus::protocol_state::EngineProtocolState>::system_tx_leader_policy(&outbe_runtime::OutbeRuntime::new());
    assert_eq!(
        (policy.step_name_or_stage)(&SystemTx::BeginBlock { step_index: 8 }),
        "cycle_tick"
    );
    let body = outbe_primitives::OutbeBlockBody::default();
    let header = outbe_primitives::OutbeHeader::default();
    let (layout, _) =
        (policy.validate_layout)(&body, &header).expect("runtime policy accepts empty genesis");
    assert!(layout.is_empty());
    let header = outbe_primitives::OutbeHeader::new(alloy_consensus::Header {
        number: 2,
        ..Default::default()
    });
    assert!(matches!(
        (policy.validate_layout)(&body, &header),
        Err(outbe_evm::system_tx::binding::LayoutBindingError::Set(
            outbe_evm::system_tx::SystemTxError::ActiveSystemTxSetMismatch { .. }
        ))
    ));
    let orders = outbe_runtime::BLOCK_ORDER;
    let describe_shape = |step: &outbe_evm::system_tx::ProtocolStep| {
        (
            step.revert_fails_block,
            step.include_validator_snapshot,
            step.needs_cycle_active_utc_day,
        )
    };
    let runtime: Vec<_> = orders
        .consensus
        .iter()
        .chain(orders.begin_block)
        .chain(orders.end_block)
        .collect();
    let lower: Vec<_> = TEST_BLOCK_ORDER
        .consensus
        .iter()
        .chain(TEST_BLOCK_ORDER.begin_block)
        .chain(TEST_BLOCK_ORDER.end_block)
        .collect();
    assert_eq!(
        runtime.iter().map(|step| step.name).collect::<Vec<_>>(),
        vec![
            "certified_parent_accounting",
            "late_finalize_credits",
            "boundary_outcome",
            "tee_bootstrap",
            "validator_liveness",
            "radicle_publication",
            "vote",
            "update",
            "enclave_upgrade_deadline",
            "rewards",
            "staking",
            "oracle",
            "oracle_slash",
            "hyperlane_slash",
            "cycle_tick",
            "nod_factory",
            "gem_factory",
            "intex_factory",
            "credis_factory",
            "gem_cleanup",
        ]
    );
    assert_eq!(
        orders
            .consensus
            .iter()
            .map(|step| step.name)
            .collect::<Vec<_>>(),
        TEST_BLOCK_ORDER
            .consensus
            .iter()
            .map(|step| step.name)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        runtime
            .iter()
            .map(|step| describe_shape(step))
            .collect::<Vec<_>>(),
        lower
            .iter()
            .map(|step| describe_shape(step))
            .collect::<Vec<_>>()
    );
    for (regime, base) in [
        (
            BlockHeightRegime::Genesis,
            [false, false, false, false, false, false],
        ),
        (
            BlockHeightRegime::GenesisBootstrap,
            [false, false, true, true, true, false],
        ),
        (
            BlockHeightRegime::SteadyState,
            [true, true, false, false, true, false],
        ),
    ] {
        for flags in all_flags() {
            let mut active = base;
            active[2] |= flags.has_boundary_outcome;
            active[5] = flags.has_publication;
            for (index, (step, test_step)) in runtime.iter().zip(&lower).enumerate() {
                let expected_active = active.get(index).copied().unwrap_or(true);
                assert_eq!(
                    (step.is_active)(regime, flags),
                    expected_active,
                    "{} {regime:?} {flags:?}",
                    step.name
                );
                assert_eq!(
                    (test_step.is_active)(regime, flags),
                    expected_active,
                    "{} {regime:?} {flags:?}",
                    step.name
                );
            }
        }
    }
}

fn all_flags() -> impl Iterator<Item = outbe_primitives::block::BlockSystemFlags> {
    [false, true].into_iter().flat_map(|has_boundary_outcome| {
        [false, true].map(
            |has_publication| outbe_primitives::block::BlockSystemFlags {
                has_boundary_outcome,
                has_publication,
            },
        )
    })
}

#[test]
fn runtime_plan_and_fsm_preserve_static_indices_across_filtering() {
    use outbe_evm::system_tx::BlockSystemPlan;
    let order = &outbe_runtime::BLOCK_ORDER;
    for block in [0, 1, 2] {
        for flags in all_flags() {
            let plan = BlockSystemPlan::for_block(block, flags, order);
            let expected_consensus: Vec<u8> = match block {
                0 => vec![],
                1 => vec![2, 3, 4],
                _ => {
                    if flags.has_boundary_outcome {
                        vec![0, 1, 2, 4]
                    } else {
                        vec![0, 1, 4]
                    }
                }
            }
            .into_iter()
            .chain((block != 0 && flags.has_publication).then_some(5))
            .collect();
            assert_eq!(
                plan.consensus_steps
                    .iter()
                    .map(|step| step.id.step_index)
                    .collect::<Vec<_>>(),
                expected_consensus
            );
            assert_eq!(plan.begin_block_steps.len(), if block == 0 { 0 } else { 9 });
            assert_eq!(plan.end_block_steps.len(), if block >= 2 { 5 } else { 0 });
            assert_eq!(plan.allows_user_txs, block >= 2);
            assert_fsm_reaches_completed(plan);
        }
    }
}

fn assert_fsm_reaches_completed(plan: outbe_evm::system_tx::BlockSystemPlan) {
    use {
        outbe_evm::system_tx::BlockExecutionFsm, outbe_primitives::system_tx::BlockExecutionStage,
    };
    let block = plan.block_number;
    let mut fsm = BlockExecutionFsm::new(plan.clone());
    if block != 0 {
        assert!(fsm.on_user_tx().is_err());
    }
    for (body_index, step) in plan
        .consensus_steps
        .iter()
        .chain(&plan.begin_block_steps)
        .enumerate()
    {
        assert_eq!(fsm.expected_prefix_body_index(), Some(body_index));
        assert_eq!(fsm.expected_step_id(), Some(step.id));
        let input = if step.id.stage == BlockExecutionStage::Consensus {
            SystemTx::Consensus {
                step_index: step.id.step_index,
                payload: Default::default(),
            }
        } else {
            SystemTx::BeginBlock {
                step_index: step.id.step_index,
            }
        };
        fsm.on_system_tx(&input).unwrap();
    }
    if block >= 2 {
        assert_eq!(fsm.stage(), BlockExecutionStage::MidBlock);
        assert!(fsm.finish().is_err());
        fsm.on_user_tx().unwrap();
        fsm.enter_end_block(block).unwrap();
    }
    for step in &plan.end_block_steps {
        assert_eq!(fsm.expected_ordinal(), Some(24 + step.id.step_index));
        fsm.on_system_tx(&SystemTx::EndBlock {
            step_index: step.id.step_index,
        })
        .unwrap();
    }
    assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
    fsm.finish().unwrap();
}
