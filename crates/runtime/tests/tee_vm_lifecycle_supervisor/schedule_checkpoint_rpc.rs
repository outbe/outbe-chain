//! Real provider-backed historical state tests for `outbe_teeRenewalScheduleV1`.
use crate::chain_storage;
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    error::PrecompileError,
    storage::{
        hashmap::HashMapStorageProvider,
        readonly::{ReadOnlyStorageProvider, StorageReader},
        StorageHandle,
    },
    tee_operator_v1::TeeRenewalScheduleV1,
    validator_dto::EpochSnapshot,
    OutbeHeader, OutbePrimitives,
};
use outbe_rpc::{state_reader::OutbeRpcStateReader, OutbeApiHandler, OutbeApiServer as _};
use outbe_runtime::OutbeRuntime;
use outbe_validatorset::{schema::ValidatorSet, test_support::StorageOverrides as _};
use reth_chainspec::ChainSpec;
use reth_primitives_traits::SealedHeader;
use reth_provider::{
    providers::{BlockchainProvider, ProviderNodeTypes},
    test_utils::create_test_provider_factory_with_node_types,
    StateProviderFactory,
};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

const DKG_PREPARE_WINDOW_BLOCKS: u64 = 4;
const MINIMUM_BLOCK_TIME_MILLIS: u64 = 1_000;

struct StateBoxReader<'a>(&'a reth_storage_api::StateProviderBox);

impl StorageReader for StateBoxReader<'_> {
    fn read_storage(&self, address: Address, key: B256) -> Result<U256, PrecompileError> {
        self.0
            .storage(address, key)
            .map(|value| value.unwrap_or(U256::ZERO))
            .map_err(|error| PrecompileError::Storage(error.to_string()))
    }
}

fn epoch_storage(epoch: EpochSnapshot) -> HashMap<(Address, U256), U256> {
    let mut mem = HashMapStorageProvider::new(crate::TEST_CHAIN_ID);
    StorageHandle::enter(&mut mem, |storage| {
        ValidatorSet::new(storage).test_set_epoch_snapshot(epoch)
    })
    .expect("seed epoch snapshot in HashMapStorageProvider");
    mem.storage
}

fn read_epoch_from_provider<N>(
    provider: &BlockchainProvider<N>,
    runtime: &OutbeRuntime,
    hash: B256,
) -> EpochSnapshot
where
    N: ProviderNodeTypes<ChainSpec = ChainSpec<OutbeHeader>, Primitives = OutbePrimitives>,
{
    let state = provider
        .state_by_block_hash(hash)
        .expect("historical state must exist for canonical block");
    let mut readonly = ReadOnlyStorageProvider::new(StateBoxReader(&state));
    let reader: &dyn OutbeRpcStateReader = runtime;
    reader
        .epoch_snapshot(StorageHandle::new(&mut readonly))
        .expect("runtime reader must decode epoch snapshot")
}

struct HistoricalScheduleFixture<N: ProviderNodeTypes> {
    provider: Arc<BlockchainProvider<N>>,
    runtime: Arc<OutbeRuntime>,
    headers: Vec<SealedHeader<OutbeHeader>>,
    epochs: [EpochSnapshot; 4],
    storages: Vec<HashMap<(Address, U256), U256>>,
}

fn build_historical_schedule_fixture() -> HistoricalScheduleFixture<
    impl ProviderNodeTypes<ChainSpec = ChainSpec<OutbeHeader>, Primitives = OutbePrimitives>,
> {
    let epochs = [
        EpochSnapshot {
            number: U256::from(0_u64),
            start_timestamp: 1_700_000_000,
            start_block: 0,
            length_blocks: 10,
        },
        EpochSnapshot {
            number: U256::from(1_u64),
            start_timestamp: 1_700_000_010,
            start_block: 10,
            length_blocks: 12,
        },
        EpochSnapshot {
            number: U256::from(2_u64),
            start_timestamp: 1_700_000_020,
            start_block: 22,
            length_blocks: 25,
        },
        EpochSnapshot {
            number: U256::from(3_u64),
            start_timestamp: 1_700_000_030,
            start_block: 47,
            length_blocks: 30,
        },
    ];
    let timestamps = [1_700_000_000, 1_700_000_015, 1_700_000_025, 1_700_000_035];
    let storages: Vec<_> = epochs.iter().copied().map(epoch_storage).collect();
    let blocks: Vec<_> = timestamps
        .into_iter()
        .zip(storages.iter().cloned())
        .collect();

    let factory = create_test_provider_factory_with_node_types::<outbe_execution::OutbeNode>(
        Arc::new(ChainSpec::<OutbeHeader>::default()),
    );
    let (provider, headers) = chain_storage::seed_historical_storage_chain(factory, &blocks, 2)
        .expect("seed real historical storage chain");
    let provider = Arc::new(provider);
    let runtime = Arc::new(OutbeRuntime::new());

    // Hard precondition: verify real provider + real runtime reader return
    // independently different epoch storage at each canonical block hash.
    for (idx, expected_epoch) in epochs.iter().enumerate() {
        let observed = read_epoch_from_provider(&provider, &runtime, headers[idx].hash());
        assert_eq!(
            observed, *expected_epoch,
            "real provider historical epoch mismatch at block {idx}"
        );
    }
    let h_epoch = read_epoch_from_provider(&provider, &runtime, headers[1].hash());
    let f_epoch = read_epoch_from_provider(&provider, &runtime, headers[2].hash());
    assert_ne!(
        h_epoch, f_epoch,
        "precondition failed: H and F must have different real epoch storage"
    );

    HistoricalScheduleFixture {
        provider,
        runtime,
        headers,
        epochs,
        storages,
    }
}

impl<N> HistoricalScheduleFixture<N>
where
    N: ProviderNodeTypes<ChainSpec = ChainSpec<OutbeHeader>, Primitives = OutbePrimitives>,
{
    fn rpc_module(&self) -> jsonrpsee::Methods {
        let reader: Arc<dyn OutbeRpcStateReader> = self.runtime.clone();
        OutbeApiHandler::new(Arc::clone(&self.provider), reader)
            .with_tee_renewal_schedule(DKG_PREPARE_WINDOW_BLOCKS, MINIMUM_BLOCK_TIME_MILLIS)
            .into_rpc()
            .into()
    }

    fn expected_schedule(&self, block_index: usize) -> TeeRenewalScheduleV1 {
        let header = &self.headers[block_index];
        let epoch = self.epochs[block_index];
        let planned = epoch.start_block + u64::from(epoch.length_blocks);
        let prepare = DKG_PREPARE_WINDOW_BLOCKS.min(u64::from(epoch.length_blocks));
        TeeRenewalScheduleV1 {
            finalized_height: header.header().inner.number,
            finalized_hash: header.hash(),
            finalized_timestamp: header.header().inner.timestamp,
            epoch_number: epoch.number.to::<u64>(),
            epoch_start_height: epoch.start_block,
            epoch_length_blocks: epoch.length_blocks,
            next_freeze_height: planned.saturating_sub(prepare),
            planned_activation_height: planned,
            dkg_prepare_window_blocks: prepare,
            minimum_block_time_millis: MINIMUM_BLOCK_TIME_MILLIS,
        }
    }
}

async fn raw_rpc_json(module: &jsonrpsee::Methods, request: Value) -> Value {
    let raw = serde_json::to_string(&request).expect("serialize request");
    let (response, _) = module
        .raw_json_request(&raw, 1)
        .await
        .expect("execute raw JSON-RPC request");
    serde_json::from_str(response.get()).expect("parse JSON-RPC response")
}

async fn request_schedule_ok(module: &jsonrpsee::Methods, request: Value) -> TeeRenewalScheduleV1 {
    let response = raw_rpc_json(module, request).await;
    assert!(
        response.get("error").is_none(),
        "expected Ok JSON-RPC response, got: {response}"
    );
    serde_json::from_value(response["result"].clone()).expect("decode TeeRenewalScheduleV1")
}

async fn request_schedule_err(module: &jsonrpsee::Methods, request: Value) -> (i64, String) {
    let response = raw_rpc_json(module, request).await;
    let error = response
        .get("error")
        .unwrap_or_else(|| panic!("expected JSON-RPC error response, got: {response}"));
    let code = error["code"].as_i64().expect("error code must be i64");
    let message = error["message"]
        .as_str()
        .expect("error message must be string")
        .to_owned();
    (code, message)
}

#[tokio::test]
async fn schedule_rpc_selects_historical_finalized_checkpoint_epoch_state_across_epoch_boundary() {
    let fixture = build_historical_schedule_fixture();
    let module = fixture.rpc_module();
    let h_hash = fixture.headers[1].hash();
    let expected_h = fixture.expected_schedule(1);
    let expected_f = fixture.expected_schedule(2);
    assert_ne!(expected_h.epoch_number, expected_f.epoch_number);
    assert_ne!(
        expected_h.planned_activation_height,
        expected_f.planned_activation_height
    );

    let observed_h = request_schedule_ok(
        &module,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "outbe_teeRenewalScheduleV1",
            "params": [h_hash],
        }),
    )
    .await;
    assert_eq!(observed_h, expected_h);

    // Advance finalized head from block 2 (epoch 2) to block 3 (epoch 3) and
    // verify the historical checkpoint H still returns block 1's exact state.
    chain_storage::set_finalized_block(&fixture.provider, fixture.headers[3].clone())
        .expect("advance finalized block to 3");
    let observed_h_after_advance = request_schedule_ok(
        &module,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "outbe_teeRenewalScheduleV1",
            "params": [h_hash],
        }),
    )
    .await;
    assert_eq!(observed_h_after_advance, expected_h);

    // Omitted selector now observes the new finalized head (block 3, epoch 3).
    let observed_new_f = request_schedule_ok(
        &module,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "outbe_teeRenewalScheduleV1",
            "params": [],
        }),
    )
    .await;
    assert_eq!(observed_new_f, fixture.expected_schedule(3));
}

#[tokio::test]
async fn schedule_rpc_wire_compatibility_omitted_empty_null_current_and_named_aliases() {
    let fixture = build_historical_schedule_fixture();
    let module = fixture.rpc_module();
    let h_hash = fixture.headers[1].hash();
    let f_hash = fixture.headers[2].hash();
    let expected_h = fixture.expected_schedule(1);
    let expected_f = fixture.expected_schedule(2);

    let omitted = request_schedule_ok(
        &module,
        json!({"jsonrpc": "2.0", "id": 1, "method": "outbe_teeRenewalScheduleV1"}),
    )
    .await;
    let empty_array = request_schedule_ok(
        &module,
        json!({"jsonrpc": "2.0", "id": 2, "method": "outbe_teeRenewalScheduleV1", "params": []}),
    )
    .await;
    let null_array = request_schedule_ok(
        &module,
        json!({"jsonrpc": "2.0", "id": 3, "method": "outbe_teeRenewalScheduleV1", "params": [null]}),
    )
    .await;
    let empty_object = request_schedule_ok(
        &module,
        json!({"jsonrpc": "2.0", "id": 4, "method": "outbe_teeRenewalScheduleV1", "params": {}}),
    )
    .await;
    let explicit_f = request_schedule_ok(
        &module,
        json!({"jsonrpc": "2.0", "id": 5, "method": "outbe_teeRenewalScheduleV1", "params": [f_hash]}),
    )
    .await;
    let snake_named_h = request_schedule_ok(
        &module,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "outbe_teeRenewalScheduleV1",
            "params": {"checkpoint_hash": h_hash}
        }),
    )
    .await;
    let camel_named_h = request_schedule_ok(
        &module,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "outbe_teeRenewalScheduleV1",
            "params": {"checkpointHash": h_hash}
        }),
    )
    .await;

    assert_eq!(omitted, expected_f);
    assert_eq!(empty_array, expected_f);
    assert_eq!(null_array, expected_f);
    assert_eq!(empty_object, expected_f);
    assert_eq!(explicit_f, expected_f);
    assert_eq!(snake_named_h, expected_h);
    assert_eq!(camel_named_h, expected_h);
}

#[tokio::test]
async fn schedule_rpc_rejects_malformed_unknown_future_and_accepts_real_genesis_hash() {
    let fixture = build_historical_schedule_fixture();
    let module = fixture.rpc_module();
    let genesis_hash = fixture.headers[0].hash();
    let above_f_hash = fixture.headers[3].hash();
    let unknown_hash = B256::repeat_byte(0xde);

    // Real genesis hash is valid when canonical and finalized.
    let genesis_schedule = request_schedule_ok(
        &module,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "outbe_teeRenewalScheduleV1",
            "params": [genesis_hash]
        }),
    )
    .await;
    assert_eq!(genesis_schedule, fixture.expected_schedule(0));

    // Malformed selector values fail with Invalid Params (-32602).
    for (id, params) in [
        (2, json!(["not-a-hash"])),
        (3, json!([123])),
        (4, json!(["0x1234"])),
        (5, json!({"checkpoint_hash": 42})),
    ] {
        let (code, _) = request_schedule_err(
            &module,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "outbe_teeRenewalScheduleV1",
                "params": params
            }),
        )
        .await;
        assert_eq!(code, -32602, "expected Invalid Params for id {id}");
    }

    // Unknown / orphan hash fails with Invalid Params (-32602).
    let (unknown_code, unknown_msg) = request_schedule_err(
        &module,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "outbe_teeRenewalScheduleV1",
            "params": [unknown_hash]
        }),
    )
    .await;
    assert_eq!(unknown_code, -32602);
    assert!(
        unknown_msg.contains(&unknown_hash.to_string()),
        "unexpected unknown-hash message: {unknown_msg}"
    );

    // Zero hash has no alias meaning; it is rejected as unknown.
    let (zero_code, _) = request_schedule_err(
        &module,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "outbe_teeRenewalScheduleV1",
            "params": [B256::ZERO]
        }),
    )
    .await;
    assert_eq!(zero_code, -32602);

    // Canonical block above finalized F fails with HeightAboveFinalized (-32602).
    let (future_code, future_msg) = request_schedule_err(
        &module,
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "outbe_teeRenewalScheduleV1",
            "params": [above_f_hash]
        }),
    )
    .await;
    assert_eq!(future_code, -32602);
    assert!(
        future_msg.contains("height 3 is above the node's finalized height 2"),
        "unexpected future-block message: {future_msg}"
    );
}

#[tokio::test]
async fn schedule_rpc_concurrent_requests_keep_independent_targets() {
    let fixture = build_historical_schedule_fixture();
    let module = fixture.rpc_module();
    let g_hash = fixture.headers[0].hash();
    let h_hash = fixture.headers[1].hash();
    let f_hash = fixture.headers[2].hash();

    let (g, h, f, default_f) = tokio::join!(
        request_schedule_ok(
            &module,
            json!({"jsonrpc": "2.0", "id": 1, "method": "outbe_teeRenewalScheduleV1", "params": [g_hash]})
        ),
        request_schedule_ok(
            &module,
            json!({"jsonrpc": "2.0", "id": 2, "method": "outbe_teeRenewalScheduleV1", "params": [h_hash]})
        ),
        request_schedule_ok(
            &module,
            json!({"jsonrpc": "2.0", "id": 3, "method": "outbe_teeRenewalScheduleV1", "params": [f_hash]})
        ),
        request_schedule_ok(
            &module,
            json!({"jsonrpc": "2.0", "id": 4, "method": "outbe_teeRenewalScheduleV1", "params": []})
        ),
    );

    assert_eq!(g, fixture.expected_schedule(0));
    assert_eq!(h, fixture.expected_schedule(1));
    assert_eq!(f, fixture.expected_schedule(2));
    assert_eq!(default_f, fixture.expected_schedule(2));

    // Also append a new block 4 with epoch 4 and confirm H remains unchanged.
    let epoch4 = EpochSnapshot {
        number: U256::from(4_u64),
        start_timestamp: 1_700_000_040,
        start_block: 77,
        length_blocks: 40,
    };
    let storage4 = epoch_storage(epoch4);
    let block4 = chain_storage::append_historical_block(
        &fixture.provider,
        4,
        1_700_000_045,
        fixture.headers[3].hash(),
        &fixture.storages[3],
        &storage4,
    )
    .expect("append block 4");
    chain_storage::set_finalized_block(&fixture.provider, block4.clone())
        .expect("advance finalized to block 4");

    let h_again = request_schedule_ok(
        &module,
        json!({"jsonrpc": "2.0", "id": 5, "method": "outbe_teeRenewalScheduleV1", "params": [h_hash]}),
    )
    .await;
    assert_eq!(h_again, fixture.expected_schedule(1));
}

#[tokio::test]
async fn schedule_rpc_preserves_config_error_precedence_for_explicit_hash() {
    let fixture = build_historical_schedule_fixture();
    let unknown_hash = B256::repeat_byte(0xab);

    let reader: Arc<dyn OutbeRpcStateReader> = fixture.runtime.clone();
    let unconfigured: jsonrpsee::Methods =
        OutbeApiHandler::new(Arc::clone(&fixture.provider), Arc::clone(&reader))
            .into_rpc()
            .into();
    let (code, msg) = request_schedule_err(
        &unconfigured,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "outbe_teeRenewalScheduleV1",
            "params": [unknown_hash]
        }),
    )
    .await;
    assert_eq!(code, -32603);
    assert!(
        msg.contains("TEE renewal schedule is not configured"),
        "unexpected unconfigured error: {msg}"
    );

    let zero_block_time: jsonrpsee::Methods =
        OutbeApiHandler::new(Arc::clone(&fixture.provider), reader)
            .with_tee_renewal_schedule(DKG_PREPARE_WINDOW_BLOCKS, 0)
            .into_rpc()
            .into();
    let (code, msg) = request_schedule_err(
        &zero_block_time,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "outbe_teeRenewalScheduleV1",
            "params": [unknown_hash]
        }),
    )
    .await;
    assert_eq!(code, -32603);
    assert!(
        msg.contains("TEE renewal schedule has zero minimum block time"),
        "unexpected zero-min-block-time error: {msg}"
    );
}
