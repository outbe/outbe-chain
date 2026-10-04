use super::*;

const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const REQUIRED_HEADROOM_BPS: u64 = 2_000;
const BPS_DENOMINATOR: u64 = 10_000;

fn capacity_context(
    seed_hash: B256,
    parent_metadata: CertifiedParentAccountingMetadata,
    proposer: Address,
) -> OutbeBlockExecutionCtx<'static> {
    fixtures::parent_accounting_context(fixtures::ParentAccountingFixture {
        parent_hash: seed_hash,
        metadata: parent_metadata,
        artifact: AccountedParentArtifact {
            summary: ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            },
            timestamp: 0,
            state_root: Some(B256::repeat_byte(0x91)),
        },
        proposer,
    })
}

fn assert_capacity_headroom(receipt: &Receipt, visible_gas: u64) {
    let maximum_used =
        BLOCK_GAS_LIMIT * (BPS_DENOMINATOR - REQUIRED_HEADROOM_BPS) / BPS_DENOMINATOR;
    eprintln!(
        "CapacityForfeiture CycleTick visible gas: used={visible_gas}, max_for_20pct_headroom={maximum_used}"
    );
    assert!(receipt.success);
    assert!(
        visible_gas <= maximum_used,
        "CapacityForfeiture CycleTick visible gas {visible_gas} leaves less than 20% headroom"
    );
}

fn assert_capacity_events(receipt: &Receipt) {
    let capacity_event = keccak256(
        "WorldwideDayCapacityForfeited(uint32,uint32,uint32,uint256,uint256,uint256,bytes32,uint32,uint256,uint64,uint64,uint8,uint64)",
    );
    assert!(receipt.logs.iter().any(|log| {
        log.address == outbe_primitives::addresses::METADOSIS_ADDRESS
            && log.data.topics().first() == Some(&capacity_event)
    }));
    let retirement_event = keccak256("TributePartitionRetired(uint32)");
    assert!(receipt.logs.iter().any(|log| {
        log.address == outbe_primitives::addresses::TRIBUTE_ADDRESS
            && log.data.topics().first() == Some(&retirement_event)
    }));
    let capacity_log = receipt
        .logs
        .iter()
        .find_map(|log| {
            outbe_metadosis::precompile::IMetadosis::WorldwideDayCapacityForfeited::decode_log(log)
                .ok()
        })
        .expect("typed capacity-forfeiture event");
    assert_eq!(capacity_log.forfeitedTributeCount, u32::MAX);
    assert_eq!(capacity_log.forfeitedTributeNominalMinor, U256::MAX);
    assert_eq!(capacity_log.retirementOutcome, 2);
}

fn run_capacity_cycle() -> (u64, Receipt, B256, B256, tempfile::TempDir) {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let CapacityParent {
        mut state,
        tree_directory,
        tree_service,
        seed_hash,
        seed_root,
        body_reader,
        fire_at,
    } = prepare_capacity_parent(proposer).expect("prepare populated capacity parent");

    let mut evm_env = test_evm_env(2, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(fire_at);
    let config = OutbeEvmConfig::new_with_runtime_body_readers(
        test_chain_spec(),
        RuntimeBodyReaders::new(body_reader),
    )
    .with_evm_signer(signer.clone())
    .with_compressed_tree_service(tree_service);
    let mut parent_metadata = metadata_with(vec![proposer], vec![1], Vec::new());
    parent_metadata.finalized_block_number = 1;
    parent_metadata.finalized_block_hash = seed_hash;
    let evm = config.evm_with_env(&mut state, evm_env);
    let execution = capacity_context(seed_hash, parent_metadata.clone(), proposer);
    let mut executor = config.create_executor(evm, execution);
    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution changes should apply");
    });
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash: seed_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(parent_metadata),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    let cycle = fixtures::observe_cycle_tick(system_txs, "valid begin-zone system tx", |tx| {
        let output = executor
            .execute_transaction(tx)
            .expect("begin-zone prefix through CapacityForfeiture CycleTick must execute");
        fixtures::CycleObservation {
            visible_gas: output.tx_gas_used(),
            internal_gas: 0,
            receipt_index: executor.receipts().len() - 1,
        }
    });
    let visible_gas = cycle
        .as_ref()
        .map(|cycle| cycle.visible_gas)
        .expect("CycleTick visible gas");
    let cycle_receipt_index = cycle
        .map(|cycle| cycle.receipt_index)
        .expect("CycleTick receipt index");
    assert_capacity_headroom(&executor.receipts()[cycle_receipt_index], visible_gas);
    assert_capacity_events(&executor.receipts()[cycle_receipt_index]);
    let receipt = executor.receipts()[cycle_receipt_index].clone();
    drop(executor);
    (
        visible_gas,
        receipt,
        post_state_root(&state.bundle_state),
        seed_root,
        tree_directory,
    )
}

pub(super) fn run() {
    let proposer = run_capacity_cycle();
    let replay = run_capacity_cycle();
    assert_eq!(
        proposer.0, replay.0,
        "same-parent replay must reproduce visible gas"
    );
    assert_eq!(
        proposer.1, replay.1,
        "same-parent replay must reproduce the exact receipt and events"
    );
    assert_eq!(
        proposer.2, replay.2,
        "re-executing the same CapacityForfeiture CycleTick from the same parent must reproduce gas, receipt/events, and state root"
    );
    assert_eq!(proposer.3, replay.3, "seeded parent roots must match");
}
