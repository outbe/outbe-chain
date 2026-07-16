use std::sync::Arc;

use alloy_evm::{block::BlockExecutor, eth::EthBlockExecutionCtx};
use alloy_primitives::{Bytes, B256, U256};
use outbe_compressed_entities::{
    CandidateCacheLimits, CeMdbx, CompressedTreeService, EnvironmentIdentity, FinalizedMarker,
    ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_evm::{OutbeBlockExecutionCtx, OutbeEvmConfig};
use outbe_primitives::{addresses::COMPRESSED_ENTITIES_ADDRESS, OutbeHeader};
use reth_ethereum::{
    chainspec::{ChainSpec, EthChainSpec, MAINNET},
    evm::revm::db::State,
};
use reth_evm::{execute::ProviderError, ConfigureEvm, EvmEnv};
use revm::{
    context::{BlockEnv, CfgEnv},
    database::CacheDB,
    database_interface::EmptyDBTyped,
    primitives::hardfork::SpecId,
    state::AccountInfo,
};

fn test_chain_spec() -> Arc<ChainSpec<OutbeHeader>> {
    MAINNET.as_ref().clone().map_header(OutbeHeader::new).into()
}

#[test]
fn create_executor_activates_the_factory_scope_against_the_exact_parent_tree() {
    let parent_hash = B256::repeat_byte(0x42);
    let parent_root = B256::with_last_byte(0x09);
    let directory = tempfile::tempdir().unwrap();
    let db = CeMdbx::open(
        directory.path(),
        EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: MAINNET.chain().id(),
            genesis_hash: parent_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            tree_format: "ckb-smt-v0.6.1-poseidon-ces1".into(),
            vendor_revision: "scope-wiring-regression".into(),
        },
        FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: parent_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: parent_root,
        },
    )
    .unwrap();
    let service = Arc::new(
        CompressedTreeService::new(
            db,
            CandidateCacheLimits {
                max_candidates: 1,
                max_encoded_bytes: 1,
            },
        )
        .unwrap(),
    );
    let config = OutbeEvmConfig::new(test_chain_spec()).with_compressed_tree_service(service);

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    db.insert_account_info(
        COMPRESSED_ENTITIES_ADDRESS,
        AccountInfo {
            nonce: 1,
            ..Default::default()
        },
    );
    db.insert_account_storage(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(2_u64))
        .unwrap();
    db.insert_account_storage(
        COMPRESSED_ENTITIES_ADDRESS,
        U256::from(1_u64),
        U256::from_be_bytes(parent_root.0),
    )
    .unwrap();
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(MAINNET.chain().id())
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(1_u64),
            gas_limit: 30_000_000,
            beneficiary: outbe_primitives::addresses::REWARDS_ADDRESS,
            timestamp: U256::from(1_u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, env);
    let ctx = OutbeBlockExecutionCtx {
        inner: EthBlockExecutionCtx {
            parent_hash,
            parent_beacon_block_root: None,
            ommers: &[],
            withdrawals: None,
            extra_data: Bytes::new(),
            tx_count_hint: Some(0),
            slot_number: None,
        },
        timestamp_millis_part: 0,
        block_hash: None,
        expected_begin_system_txs: Vec::new(),
        expected_end_system_txs: Vec::new(),
        system_layout_error: None,
        parent_consensus_metadata: None,
        proposer_evm_address: None,
        execute_outbe_block_hooks: false,
        prebuilt_phase1_tx: None,
        parent_artifact_hint: None,
        pending_tee_bootstrap: None,
        execution_read_budget: None,
    };

    let mut executor = config.create_executor(evm, ctx);
    executor.apply_pre_execution_changes().unwrap();

    // `execution_scope()` is the Arc captured when OutbeEvmFactory installed
    // the precompile dispatch closure. Seeing the lifecycle activation and the
    // non-empty exact-parent root here proves create_executor configured and
    // activated that same instance, rather than a second executor-only scope.
    let precompile_scope = executor.evm().execution_scope();
    assert_eq!(precompile_scope.parent_root().unwrap(), parent_root);
    precompile_scope.ce_work_checkpoint().unwrap();
}
