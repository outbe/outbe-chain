//! Real signed executor transactions and detached historical replay.
use super::*;
use alloy_evm::tracing::TxTracer;
use outbe_evm::executor::witness::clear_detached_trace_witness;
use revm::context_interface::{ContextTr, JournalTr};

mod cycle;
mod host;
mod system;
mod user;
use host::{Failure, IsolationHost};

const PROBE: Address = Address::repeat_byte(0x91);
const BLOCK: u64 = 2;
const PARENT: B256 = B256::repeat_byte(0x93);
type TestState = State<CacheDB<EmptyDBTyped<ProviderError>>>;

fn config(failure: Failure) -> OutbeEvmConfig {
    outbe_chain_constants::initialize(None).unwrap();
    OutbeEvmConfig::new(test_chain_spec(), IsolationHost::new(failure))
        .with_evm_signer(test_evm_signer())
}

fn seeded(seed: impl FnOnce(StorageHandle<'_>)) -> TestState {
    let proposer = test_evm_signer().address();
    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            seed_valid_tee_nodes(storage.clone(), &[proposer]);
            let registry = outbe_teeregistry::TeeRegistry::new(storage.clone());
            let node = registry.validator_v1_node_hash.read(&proposer).unwrap();
            registry
                .v1_node_attestation_ed25519
                .write(&node, B256::repeat_byte(1))
                .unwrap();
            seed(storage);
        });
    state.database.insert_account_info(
        proposer,
        AccountInfo {
            balance: U256::from(1_000_000_000u64),
            ..Default::default()
        },
    );
    state.database.insert_account_info(
        PROBE,
        AccountInfo {
            nonce: 1,
            ..Default::default()
        },
    );
    state
}

fn context(
    config: &OutbeEvmConfig,
) -> (
    OutbeBlockExecutionCtx<'static>,
    Vec<Recovered<TransactionSigned>>,
) {
    let proposer = test_evm_signer().address();
    let (ctx, metadata) = block_two_execution_ctx(None, Bytes::new(), proposer, PARENT);
    let begin = begin_system_txs_for_test(
        config,
        BeginBlockFixture {
            block_number: BLOCK,
            parent_hash: PARENT,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(metadata),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    (ctx, begin)
}

fn signed(nonce: u64, to: TxKind, input: &[u8], gas_limit: u64) -> Recovered<TransactionSigned> {
    test_evm_signer()
        .sign_eip1559(TxEip1559 {
            chain_id: CHAIN_ID,
            nonce,
            gas_limit,
            max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
            max_priority_fee_per_gas: 0,
            to,
            value: U256::ZERO,
            input: Bytes::copy_from_slice(input),
            access_list: Default::default(),
        })
        .unwrap()
        .try_into_recovered()
        .unwrap()
}

fn failure(receipt: &Receipt, code: u16) {
    assert!(!receipt.success);
    assert_eq!(receipt.cumulative_gas_used, 0);
    assert_eq!(receipt.logs.len(), 1);
    let log = &receipt.logs[0];
    assert_eq!(log.address, OUTBE_SYSTEM_TX_ADDRESS);
    let event = outbe_evm::failure_receipt::OutbeFailure::decode_log_data(&log.data).unwrap();
    assert_eq!(event.code, code);
    assert!(!event.reason.contains("secret"));
    assert!(!event.reason.contains("payload"));
}

// Dropping an executor after pre-execution is the production Reth trace entry path.
fn trace_state(config: &OutbeEvmConfig, state: &mut TestState, tx_count: usize) {
    let (mut ctx, begin) = context(config);
    ctx.expected_begin_system_txs = begin;
    ctx.block_hash = Some(B256::repeat_byte(0x96));
    ctx.inner.tx_count_hint = Some(tx_count);
    let evm = config.evm_with_env(state, test_evm_env(BLOCK, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, ctx);
    with_phase1_verify_disabled(|| executor.apply_pre_execution_changes()).unwrap();
}
