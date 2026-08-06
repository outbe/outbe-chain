//! — executor ordering contract.
//!
//! Pins the invariant that backs the slashindicator precompile's epoch-lag
//! admissibility: the precompile reads ValidatorSet's current epoch projection
//! and trusts that the value is post-bump for the current block because `transition_epoch`
//! runs in the pre-execution hook chain BEFORE any user transaction.
//!
//! The behaviour test here drives `run_outbe_pre_execution_hooks`
//! against a primed in-memory storage provider and asserts that, when
//! the block height is exactly an epoch boundary, the hook bumps
//! ValidatorSet's epoch number AND epoch start block —
//! and that these writes are visible to a fresh `ValidatorSet` facade
//! attached to the same storage after the hook returns. Because the
//! function returns control to the executor's tx-execution phase, the
//! visible-after-return contract implies "visible before user tx
//! phase".

use alloy_primitives::{Address, U256};
use outbe_evm::executor::run_outbe_pre_execution_hooks;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::EpochSnapshot;

const CHAIN_ID: u64 = 1;
const EPOCH_LENGTH: u32 = 10;
const PROPOSER: Address = Address::ZERO;

/// Seeds the minimum on-chain state the pre-exec hook chain needs to
/// reach `is_epoch_boundary` + `transition_epoch`:
///   * `config_epoch_length_blocks = EPOCH_LENGTH`
///   * `epoch_start_block = 0`
///   * `epoch_number = 1` (we expect post-call value of 2)
fn seed_validator_set(storage: StorageHandle, initial_epoch: u64) {
    let mut vs = ValidatorSet::new(storage.clone());
    vs.test_set_epoch_snapshot(EpochSnapshot {
        number: U256::from(initial_epoch),
        start_timestamp: 0,
        start_block: 0,
        length_blocks: EPOCH_LENGTH,
    })
    .unwrap();
    // Seed COEN/0xUSD pair + 1.0 rate so begin-block NOD/GEM/INTEX promotion
    // reads a registered pair instead of reverting "pair not registered".
    let mut oracle = outbe_oracle::contract::OracleContract::new(storage);
    oracle.register_pair("COEN", "0xUSD").unwrap();
    oracle
        .set_exchange_rate(
            Address::ZERO,
            "COEN",
            "0xUSD",
            U256::from(1_000_000_000_000_000_000u128),
            0,
            0,
        )
        .unwrap();
}

#[test]
fn transition_epoch_runs_in_pre_execution_before_user_txs() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let boundary_block = EPOCH_LENGTH as u64;
    provider.set_block_number(boundary_block);

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

        // (2) Drive the pre-execution hook chain. `genesis_validators
        // = None` because we are well past block 1; the genesis-state
        // validation branch is gated on `block_number <= 1`.
        let ctx = BlockRuntimeContext::new(
            BlockContext::new(
                boundary_block,
                /*timestamp=*/ 1_700_000_000,
                CHAIN_ID,
                PROPOSER,
                Vec::new(),
            ),
            storage.clone(),
        );
        run_outbe_pre_execution_hooks(&ctx, None).expect("pre-exec hook chain must succeed");

        // (3) Fresh facade on the SAME storage — proves the bump landed
        // in storage and is visible to anything that observes the
        // storage handle after the hook returns. The executor calls
        // this hook chain immediately before handing control to the
        // tx-execution loop, so "visible after return" implies
        // "visible before any user tx runs". This is the contract the
        // slashindicator precompile relies on when it reads the named current-epoch
        // projection from a fresh ValidatorSet facade.
        let vs_after = ValidatorSet::new(storage);
        let epoch_after = vs_after.epoch_snapshot().unwrap();
        assert_eq!(
            epoch_after.number,
            U256::from(2u64),
            "transition_epoch must have run inside pre-exec, bumping \
             epoch_number 1 → 2",
        );
        assert_eq!(
            epoch_after.start_block, boundary_block,
            "transition_epoch must have anchored epoch_start_block at \
             the boundary block",
        );
    });
}

/// Companion negative test: on a non-boundary block, `transition_epoch`
/// must NOT fire. Pins the trigger half of the contract — without this,
/// the positive test could pass against a runtime that calls
/// `transition_epoch` unconditionally on every pre-exec invocation,
/// which would corrupt the per-block stable-epoch assumption.
#[test]
fn transition_epoch_does_not_fire_inside_an_epoch() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let mid_epoch_block = (EPOCH_LENGTH as u64) / 2;
    provider.set_block_number(mid_epoch_block);

    provider.enter(|storage| {
        seed_validator_set(storage.clone(), 1);

        let ctx = BlockRuntimeContext::new(
            BlockContext::new(
                mid_epoch_block,
                1_700_000_000,
                CHAIN_ID,
                PROPOSER,
                Vec::new(),
            ),
            storage.clone(),
        );
        run_outbe_pre_execution_hooks(&ctx, None).expect("pre-exec hook chain must succeed");

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
