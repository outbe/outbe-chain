use alloy_consensus::{
    SignableTransaction as _, Transaction as _, TxEip1559, TxEip7702, TxReceipt as _,
};
use alloy_eips::{eip1559::MIN_PROTOCOL_BASE_FEE, eip2718::Encodable2718, eip7702::Authorization};
use alloy_evm::block::{BlockExecutionError, BlockValidationError, CommitChanges};
use alloy_evm::eth::{EthBlockExecutionCtx, EthBlockExecutor};
use alloy_evm::{Evm as _, RecoveredTx as _};
use alloy_primitives::{
    address, keccak256, logs_bloom, Address, Bytes, Log, Signature, TxKind, B256, U256,
};
use alloy_sol_types::{SolCall, SolEvent};
use fixtures::{
    begin_system_txs_for_test, block_one_execution_ctx, boundary_with, boundary_with_epoch,
    cache_db_from_storage, dummy_pubkey, execution_ctx, execution_ctx_with_tee_bootstrap,
    metadata_with, numbered_test_address, post_state_root, register_and_activate_native,
    sample_tee_bootstrap_payload, sample_tee_bootstrap_payload_at,
    sample_tee_bootstrap_payload_for, seed_cycle_tick_genesis, seed_previous_day_vwap,
    seed_registered_active_validator, seed_test_committee_snapshot, signer_balance,
    state_with_active_proposer, state_with_active_proposer_and_funded_account,
    state_with_active_validators_seeded, state_with_active_validators_seeded_at_block,
    state_with_active_validators_seeded_at_block_with_cycle_frames, test_chain_spec,
    test_chain_spec_with_genesis_timestamp, test_evm_env, test_evm_signer, test_metadata,
    test_oracle_submit_vote_tx, test_oracle_submit_vote_tx_with_gas_limit, test_register_active,
    test_register_waiting, test_regular_tx, BeginBlockFixture, BootstrapFixture, CHAIN_ID, OWNER,
    TEST_BLOCK_TIMESTAMP_BASE,
};
pub use fixtures::{block_two_execution_ctx, seed_valid_tee_nodes};
use k256::ecdsa::signature::hazmat::PrehashSigner as _;
use outbe_evm::executor::*;
use outbe_evm::executor::{
    AccountedParentArtifact, AccountedParentArtifactProvider, OutbeBlockExecutor,
    SystemFailureReceiptInput,
};
use outbe_evm::{
    config::{OutbeBlockExecutionCtx, OutbeEvmConfig},
    signer::{OutbeEvmSigner, SharedOutbeEvmSigner},
    system_tx::{
        build_unsigned_system_tx, is_reserved_system_tx, system_tx_intrinsic_gas, SystemTx,
    },
};
use outbe_nod::{precompile::INod, NodContract, NodItemState};
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::consensus::{ConsensusExecutionBridge, GenesisValidator, GenesisValidators};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::reshare_artifact::{
    decode_outbe_block_artifacts, encode_outbe_block_artifacts, ConsensusHeaderArtifact,
    ExecutionSummaryArtifact, OutbeBlockArtifacts,
};
use outbe_primitives::storage::{
    direct::DirectStorageProvider, hashmap::HashMapStorageProvider, StorageHandle,
};
use outbe_primitives::tee_genesis_v1::GRAMINE_DIRECT_DEV_CHAIN_ID;
use outbe_primitives::OutbeHeader;
use outbe_stablecoin::StablecoinContract;
use outbe_stablecoinfactory::{precompile::IStablecoinFactory, StablecoinFactoryContract};
use outbe_system_common::addresses::{
    CYCLE_ADDRESS, NOD_ADDRESS, ORACLE_ADDRESS, OUTBE_SYSTEM_TX_ADDRESS, REWARDS_ADDRESS,
    SLASH_INDICATOR_ADDRESS, STABLECOIN_FACTORY_ADDRESS, STABLECOIN_POLICY_REGISTRY_ADDRESS,
    STAKING_ADDRESS, UPDATE_ADDRESS, VOTE_ADDRESS,
};
use outbe_system_common::stablecoin::{
    encode_canonical_stablecoin_create, StablecoinCreatePayload,
};
use outbe_system_common::stablecoin_fork::STABLECOIN_CREATE_BOND;
use outbe_system_common::time::WorldwideDay;
use outbe_tee::witness::decode_witness_payload_v1;
use outbe_tribute::{TributeContract, TributeData};
use outbe_validatorset::{HistoryCounters, ValidatorHistory};
use outbe_vote::{
    constants::VOTING_WINDOW_BLOCKS,
    precompile::IVote,
    schema::{BondState, ProposalStatus, Vote},
};
use reth_ethereum::chainspec::{ChainSpec, ChainSpecBuilder, MAINNET};
use reth_ethereum::evm::revm::db::State;
use reth_ethereum::{Receipt, TransactionSigned};
use reth_evm::{block::BlockExecutor, execute::ProviderError, ConfigureEvm, EvmEnv};
use reth_primitives_traits::{Recovered, SignedTransaction as _};
use revm::context::result::{ExecutionResult, HaltReason, InvalidTransaction, OutOfGasError};
use revm::context_interface::Block as _;
use revm::{
    context::{BlockEnv, CfgEnv, TxEnv},
    database::states::bundle_state::BundleRetention,
    database::{CacheDB, Database},
    database_interface::EmptyDBTyped,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
};
use std::sync::Arc;

mod accounting;
mod begin_layout;
mod block_artifacts;
mod block_artifacts_factory_boundary;
mod block_artifacts_nod_bodies;
mod block_artifacts_proposer_parity;
mod block_artifacts_withdrawals;
mod boundary_committee_transition;
mod boundary_fixtures;
mod boundary_genesis_validation;
mod boundary_precompile_persistence;
mod boundary_system_phases;
mod eip7702_bootstrap;
mod eip7702_sponsored;
mod eip7702_support;
mod end_block_reservation;
mod end_block_rollback;
pub mod fixtures;
pub mod fixtures_execution;
mod oracle_soft_failure;
mod receipts_and_gas;
mod receipts_and_gas_begin_block_events;
mod receipts_and_gas_calibration_auction;
mod receipts_and_gas_calibration_cleanup;
mod receipts_and_gas_calibration_envelope;
mod receipts_and_gas_expiry;
mod receipts_and_gas_factory_approval;
mod receipts_and_gas_native_sinks;
mod receipts_and_gas_patch_approval;
mod receipts_and_gas_rewards;
mod receipts_and_gas_support;
mod sponsorship_quota;
mod system_execution_gas;
mod system_execution_verifier;
mod system_failure_codes;
mod timestamp;
mod witness_parity;

#[test]
fn executor_context_uses_canonical_chain_identity() {
    let chain_spec = test_chain_spec();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let mut state = State::builder()
        .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
        .with_bundle_update()
        .build();
    let mut env = test_evm_env(7, REWARDS_ADDRESS);
    env.cfg_env.chain_id = CHAIN_ID + 1;
    let evm = config.evm_with_env(&mut state, env);
    let executor = config.create_executor(evm, execution_ctx(Some(7), Bytes::new()));
    let proposer = Address::repeat_byte(0x42);
    let context = executor.block_context(7, TEST_BLOCK_TIMESTAMP_BASE, proposer);

    assert_eq!(executor.chain_id, chain_spec.chain.id());
    assert_eq!(context.chain_id, chain_spec.chain.id());
    assert_eq!(context.genesis_hash, chain_spec.genesis_hash());
    assert_eq!(context.genesis_timestamp, chain_spec.genesis_timestamp());
    assert_eq!(context.block_number, 7);
    assert_eq!(context.timestamp, TEST_BLOCK_TIMESTAMP_BASE);
    assert_eq!(context.proposer, proposer);
    assert!(context.validators.is_empty());
}
