mod size_budget;
mod stages;

use std::time::Instant;

use super::*;
use alloy_consensus::{SignableTransaction as _, TxEip1559};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{address, Bytes, Signature, TxKind, B256, U256};
use alloy_rpc_types_engine::PayloadId;
use outbe_evm::OutbeEvmSigner;
use outbe_primitives::{
    addresses::REWARDS_ADDRESS, consensus::ConsensusExecutionBridge,
    runtime_audit_v1::OTHER_PAYLOAD_EXECUTION_FAILURE, tee_genesis_v1::GRAMINE_DIRECT_DEV_CHAIN_ID,
    OutbePrimitives,
};
use reth_chainspec::{ChainSpecBuilder, EthereumHardfork, ForkCondition};
use reth_primitives_traits::{SealedHeader, SignedTransaction as _};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
use reth_transaction_pool::{
    error::InvalidPoolTransactionError,
    identifier::{SenderId, TransactionId},
    noop::NoopTransactionPool,
    EthPooledTransaction, TransactionOrigin,
};

type TestPool = NoopTransactionPool<EthPooledTransaction>;
type TestProvider = MockEthProvider<OutbePrimitives, ChainSpec<OutbeHeader>>;

#[test]
fn payload_failure_kind_is_structural_and_stable() {
    let other = BlockExecutionError::msg("unclassified payload failure");

    assert_eq!(
        payload_execution_failure_kind(&other),
        OTHER_PAYLOAD_EXECUTION_FAILURE
    );
}

const ACTIVE_PAYLOAD_BLOCK_TIMESTAMP: u64 = 1_700_000_000;

fn narrow_user_stage_fixture() -> (OutbeEvmConfig, TestProvider) {
    let mut chain_spec = ChainSpecBuilder::mainnet()
        .with_fork(EthereumHardfork::Paris, ForkCondition::Block(0))
        .build();
    chain_spec.chain = GRAMINE_DIRECT_DEV_CHAIN_ID.into();
    chain_spec.genesis.config.chain_id = GRAMINE_DIRECT_DEV_CHAIN_ID;
    let chain_spec: Arc<ChainSpec<OutbeHeader>> = chain_spec.map_header(OutbeHeader::new).into();
    let signer =
        Arc::new(OutbeEvmSigner::from_secret_bytes([1u8; 32]).expect("test proposer key is valid"));
    let evm_config = OutbeEvmConfig::new_with_bridge(
        chain_spec.clone(),
        ConsensusExecutionBridge::new(),
        outbe_evm::stub_host(),
    )
    .with_evm_signer(signer);
    let provider =
        MockEthProvider::<OutbePrimitives>::new().with_chain_spec(chain_spec.as_ref().clone());
    (evm_config, provider)
}
