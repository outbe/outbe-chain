use super::*;
use super::{
    validate_compressed_entities_root_after_seal, validate_compressed_entities_root_scheme,
    AccountedParentArtifact, AccountedParentArtifactProvider, OutbeBlockExecutor,
    SystemFailureReceiptInput,
};
use crate::{
    config::{OutbeBlockExecutionCtx, OutbeEvmConfig},
    signer::OutbeEvmSigner,
    system_tx::{
        build_unsigned_system_tx, build_unsigned_system_tx_with_gas_limit, system_tx_intrinsic_gas,
        OcompLifecycleActivation, SystemTxInputV2, SystemTxKind,
    },
};
use alloy_consensus::{TxEip1559, TxEip7702, TxReceipt as _};
use alloy_eips::{eip1559::MIN_PROTOCOL_BASE_FEE, eip2718::Encodable2718, eip7702::Authorization};
use alloy_evm::eth::{EthBlockExecutionCtx, EthBlockExecutor};
use alloy_primitives::{
    address, keccak256, logs_bloom, Address, Bytes, Log, Signature, TxKind, B256, U256,
};
use alloy_sol_types::{SolCall, SolEvent};
use fixtures::{
    begin_system_txs_for_test, begin_system_txs_for_test_with_bootstrap, block_one_execution_ctx,
    boundary_with, boundary_with_epoch, cache_db_from_storage, dummy_pubkey, execution_ctx,
    execution_ctx_with_tee_bootstrap, metadata_with, numbered_test_address, persistent_test_tree,
    persistent_test_tree_with_marker, register_and_activate_with_ocomp_registration,
    sample_tee_bootstrap_payload, sample_tee_bootstrap_payload_at,
    sample_tee_bootstrap_payload_for, seed_compressed_entities_genesis, seed_previous_day_vwap,
    seed_registered_active_validator, seed_test_committee_snapshot, seed_test_ocomp_profile,
    signer_balance, state_with_active_proposer, state_with_active_proposer_and_funded_account,
    state_with_active_proposer_and_funded_account_fixture,
    state_with_active_proposer_without_ocomp, state_with_active_validators_seeded,
    state_with_active_validators_seeded_at_block,
    state_with_active_validators_seeded_at_block_with_cycle_frames, test_chain_spec, test_evm_env,
    test_evm_signer, test_metadata, test_ocomp_fork_install, test_oracle_submit_vote_tx,
    test_oracle_submit_vote_tx_with_gas_limit, test_register_active, test_register_waiting,
    test_regular_tx, CHAIN_ID, OWNER, TEST_BLOCK_TIMESTAMP_BASE,
};
use k256::ecdsa::signature::hazmat::PrehashSigner as _;
use outbe_compressed_entities::{
    CandidateCacheLimits, CeMdbx, CeWorkConfig, CompressedTreeService, EnvironmentIdentity,
    ExactParentIdentity, ExecutionScope, FinalizedMarker, ACTIVE_COMMITMENT_SCHEME,
    LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_nod::{
    precompile::INod, NodBucketState, NodContract, NodItemState, NodRepositoryReader,
    NodRepositoryWriter,
};
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle, StorageWriterHandle};
use outbe_primitives::addresses::{
    CYCLE_ADDRESS, NOD_ADDRESS, ORACLE_ADDRESS, OUTBE_SYSTEM_TX_ADDRESS, REWARDS_ADDRESS,
    SLASH_INDICATOR_ADDRESS, STABLECOIN_FACTORY_ADDRESS, STABLECOIN_POLICY_REGISTRY_ADDRESS,
    STAKING_ADDRESS, UPDATE_ADDRESS, VOTE_ADDRESS,
};
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::consensus::{ConsensusExecutionBridge, GenesisValidator, GenesisValidators};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::hook_events::partition_hook_events;
use outbe_primitives::reshare_artifact::{
    encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, ConsensusHeaderArtifact,
    ExecutionSummaryArtifact, OutbeBlockArtifacts,
};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::tee_genesis_v1::GRAMINE_DIRECT_DEV_CHAIN_ID;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::OutbeHeader;
use outbe_primitives::{
    stablecoin::{encode_canonical_stablecoin_create, StablecoinCreatePayload},
    stablecoin_fork::STABLECOIN_CREATE_BOND,
};
use outbe_stablecoin::StablecoinContract;
use outbe_stablecoinfactory::{precompile::IStablecoinFactory, StablecoinFactoryContract};
use outbe_tribute::{TributeContract, TributeData, TributeRepositoryReader};
use outbe_validatorset::{ValidatorHistory, ValidatorLifecycle};
use outbe_vote::{
    constants::VOTING_WINDOW_BLOCKS,
    precompile::IVote,
    schema::{BondSettlement, ProposalStatus, Vote},
};
use reth_ethereum::chainspec::{ChainSpec, ChainSpecBuilder, MAINNET};
use reth_ethereum::evm::revm::db::State;
use reth_ethereum::Receipt;
use reth_evm::{block::BlockExecutor, execute::ProviderError, ConfigureEvm, EvmEnv};
use reth_primitives_traits::SignedTransaction as _;
use revm::{
    context::{BlockEnv, CfgEnv},
    database::states::bundle_state::BundleRetention,
    database::{CacheDB, Database},
    database_interface::EmptyDBTyped,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
};
use std::sync::{Arc, Mutex};

mod accounting;
mod block_artifacts;
mod boundary;
mod fixtures;
pub(super) mod harness;
mod receipts_and_gas;
mod system_execution;
mod system_failure_codes;
mod zero_fee;
