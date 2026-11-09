use super::schedule_replay::*;

use alloy_primitives::{Bytes, B256};
use outbe_primitives::tee_genesis_v1::GRAMINE_DIRECT_DEV_CHAIN_ID;
use reth_ethereum::{chainspec::MAINNET, evm::revm::db::State};
use reth_evm::{
    execute::{BlockBuilder, Executor, ProviderError},
    ConfigureEvm,
};
use reth_primitives_traits::RecoveredBlock;
use reth_provider::{HashedPostStateProvider, StateRootProvider};
use revm::{database::CacheDB, database_interface::EmptyDBTyped};

/// Regression test: header artifact (DKG boundary) must be present during
/// state-root computation on the proposer, not attached after. The bridge
/// injects the artifact via `context_for_next_block` so that proposer and
/// validator execute with identical `extra_data`.
#[test]
fn genesis_block_with_header_artifact_reexecutes_deterministically() {
    use outbe_primitives::{
        consensus::{DkgBoundaryArtifact, ReshareResult},
        reshare_artifact::{encode_consensus_header_artifact, ConsensusHeaderArtifact},
    };

    let bridge = test_bridge();

    // The seeded active set is `[GENESIS_OWNER]`. The boundary artifact must
    // carry the canonical hashes the executor recomputes in
    // `apply_boundary_outcome`. Otherwise the begin-zone BoundaryOutcome
    // system tx is rejected (active_set_hash / VRF / committee_set_hash
    // checks). Derive each value from the same canonical layout the executor
    // uses rather than stubbing magic bytes.
    let new_active_set = vec![GENESIS_OWNER];

    // `hash_boundary_active_set`: keccak256(len_be_u64 || addr_20_bytes...).
    let active_set_hash = {
        let mut bytes = Vec::with_capacity(8 + new_active_set.len() * 20);
        bytes.extend_from_slice(&(new_active_set.len() as u64).to_be_bytes());
        for address in &new_active_set {
            bytes.extend_from_slice(address.as_slice());
        }
        alloy_primitives::keccak256(bytes)
    };

    // VRF group public key commitment is keccak256 of the raw 96-byte key.
    let vrf_group_public_key_bytes = Bytes::from_static(&[0xBBu8; 96]);
    let vrf_group_public_key = alloy_primitives::keccak256(vrf_group_public_key_bytes.as_ref());

    // committee_set_hash_v2 binds the seeded consensus pubkey for
    // GENESIS_OWNER. `seed_active_validators` registers index-0 validators
    // with pubkey byte[0] = idx + 1, rest zero.
    let mut genesis_consensus_pubkey = [0u8; 48];
    genesis_consensus_pubkey[0] = 1;
    let committee_snapshot = outbe_validatorset::CommitteeSnapshot {
        committee: vec![outbe_validatorset::CommitteeEntry {
            address: GENESIS_OWNER,
            consensus_pubkey: genesis_consensus_pubkey,
        }],
        vrf_material_version: 0,
        vrf_group_public_key_bytes: vrf_group_public_key_bytes.to_vec(),
        vrf_public_polynomial_hash: alloy_primitives::B256::ZERO,
    };
    let committee_set_hash = outbe_validatorset::committee_set_hash_v2(0, &committee_snapshot);

    // Simulate the handler injecting a boundary artifact before building.
    let artifact = DkgBoundaryArtifact {
        epoch: 0,
        dkg_cycle: 0,
        freeze_height: 0,
        planned_activation_height: 0,
        target_set_hash: active_set_hash,
        vrf_material_version: 0,
        vrf_group_public_key,
        vrf_group_public_key_bytes,
        committee_set_hash,
        is_validator_set_change: true,
        outcome: Bytes::from_static(b"test-outcome"),
        is_full_dkg: true,
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: ReshareResult {
            new_active_set,
            active_set_hash,
        },
    };
    let active_set = artifact.reshare.new_active_set.clone();
    let encoded =
        encode_consensus_header_artifact(&ConsensusHeaderArtifact::BoundaryOutcome(artifact))
            .expect("artifact encoding must succeed");

    let config = test_config(bridge.clone());
    let parent = test_parent();
    let provider = DeterministicEmptyStateProvider;

    // Proposer path: build block (executor sees artifact via extra_data).
    let mut proposer_db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    seed_active_validators(&mut proposer_db, &active_set);
    assert_seeded_validators(&mut proposer_db, &active_set);
    let mut proposer_state = State::builder()
        .with_database(proposer_db)
        .with_bundle_update()
        .build();

    let mut attrs = next_block_attrs(encoded);
    attrs.inner.gas_limit = outbe_primitives::system_tx::protocol_block_gas_limit(1);
    attrs.pending_tee_bootstrap = Some(genesis_dev_tee_bootstrap(committee_set_hash));
    let mut builder = config
        .builder_for_next_block(&mut proposer_state, &parent, attrs.clone())
        .expect("builder must be created");
    builder
        .apply_pre_execution_changes()
        .expect("pre-execution changes must succeed");
    let begin_system_txs = config
        .build_begin_system_txs(
            1,
            GRAMINE_DIRECT_DEV_CHAIN_ID,
            outbe_primitives::system_tx::protocol_block_gas_limit(1),
            parent.hash(),
            &attrs.inner.extra_data,
            attrs.parent_consensus_metadata.clone(),
            attrs.proposer_evm_address,
            None,
            attrs.pending_tee_bootstrap.clone(),
        )
        .expect("begin-zone system txs must build");
    for tx in begin_system_txs {
        builder
            .execute_transaction(tx)
            .expect("begin-zone system tx must execute through builder tx loop");
    }

    let outcome = builder
        .finish(&provider, None)
        .expect("proposer path must build a block");
    let built_block = outcome.block;
    let built_hash = built_block.hash();
    let built_root = built_block.header().inner.state_root;
    let expected_cached_summary = outbe_primitives::consensus::CachedExecutionSummary {
        summary: outbe_primitives::reshare_artifact::ExecutionSummaryArtifact {
            validator_fee_sum: alloy_primitives::U256::ZERO,
        },
        timestamp: built_block.header().inner.timestamp,
        state_root: Some(built_root),
    };
    assert_eq!(
        bridge.cached_execution_summary(1, built_hash),
        Some(expected_cached_summary),
        "proposer finish must publish the canonical block's execution summary and state root",
    );

    // The block header must carry the artifact.
    assert!(
        !built_block.header().inner.extra_data.is_empty(),
        "built block must carry the header artifact in extra_data",
    );

    // Validator path: re-execute the built block.
    let mut validator_db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    seed_active_validators(&mut validator_db, &active_set);
    let validator_state = State::builder()
        .with_database(validator_db)
        .with_bundle_update()
        .build();
    let mut validator_executor = config.batch_executor(validator_state);
    validator_executor
        .execute_one(&built_block)
        .expect("validator path must re-execute the built block");

    let validator_state = validator_executor.into_state();
    let reexecuted_hashed_state = provider.hashed_post_state(&validator_state.bundle_state);
    let (reexecuted_root, _) = provider
        .state_root_with_updates(reexecuted_hashed_state.unwrap())
        .expect("re-executed state root must be computed");

    assert_eq!(
        built_root, reexecuted_root,
        "block with header artifact must re-execute to the same state root",
    );

    // Reth validates receipt and state roots only after `execute_one`
    // returns. Because the bridge cache is keyed by `(block_number, block_hash)`,
    // executing a same-height candidate that fails post-execution receipt or
    // state-root validation must never overwrite the canonical `built_hash`
    // summary or state root.
    let mut wrong_receipt_block = built_block.clone().into_block();
    wrong_receipt_block.header.inner.receipts_root = B256::repeat_byte(0x71);
    let wrong_receipt_block =
        RecoveredBlock::new_unhashed(wrong_receipt_block, built_block.senders().to_vec());
    let wrong_receipt_hash = wrong_receipt_block.header().inner.hash_slow();
    assert_ne!(wrong_receipt_hash, built_hash);

    let receipt_config = config.clone();
    let mut receipt_db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    seed_active_validators(&mut receipt_db, &active_set);
    let receipt_state = State::builder()
        .with_database(receipt_db)
        .with_bundle_update()
        .build();
    let mut receipt_executor = receipt_config.batch_executor(receipt_state);
    let receipt_result = receipt_executor
        .execute_one(&wrong_receipt_block)
        .expect("wrong receipt root is checked after successful execution");
    let receipt_validation_spec = reth_ethereum::chainspec::ChainSpecBuilder::from(&*MAINNET)
        .byzantium_activated()
        .build();
    assert!(
        reth_ethereum::consensus::validate_block_post_execution(
            &wrong_receipt_block,
            &receipt_validation_spec,
            &receipt_result,
            None,
            None,
        )
        .is_err(),
        "the fixture must reach Reth's receipt-root rejection seam",
    );

    let mut wrong_state_block = built_block.clone().into_block();
    wrong_state_block.header.inner.state_root = B256::repeat_byte(0x72);
    let wrong_state_block =
        RecoveredBlock::new_unhashed(wrong_state_block, built_block.senders().to_vec());
    let wrong_state_hash = wrong_state_block.header().inner.hash_slow();
    assert_ne!(wrong_state_hash, built_hash);

    let state_config = config.clone();
    let mut state_db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    seed_active_validators(&mut state_db, &active_set);
    let state = State::builder()
        .with_database(state_db)
        .with_bundle_update()
        .build();
    let mut state_executor = state_config.batch_executor(state);
    state_executor
        .execute_one(&wrong_state_block)
        .expect("wrong state root is checked after successful execution");
    let state = state_executor.into_state();
    let reexecuted_hashed_state = provider.hashed_post_state(&state.bundle_state);
    let (actual_state_root, _) = provider
        .state_root_with_updates(reexecuted_hashed_state.unwrap())
        .expect("state root must be computed at Reth's validation seam");
    assert_ne!(
        wrong_state_block.header().inner.state_root,
        actual_state_root,
        "the fixture must reach Reth's state-root rejection seam",
    );
    assert_eq!(
        bridge.cached_execution_summary(1, built_hash),
        Some(expected_cached_summary),
        "legitimate built-block execution summary and state root must remain intact in bridge cache",
    );
}
