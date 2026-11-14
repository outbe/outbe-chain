//! Outbe block executor.
//!
//! Wraps [`EthBlockExecutor`] and adds Outbe-specific block hooks in
//! [`apply_pre_execution_changes`](OutbeBlockExecutor::apply_pre_execution_changes).

use alloy_consensus::SignableTransaction as _;
use alloy_consensus::Transaction as _;
use alloy_eips::eip7685::Requests;
use alloy_evm::{
    block::{
        state_changes::post_block_balance_increments, BlockExecutionError, BlockExecutor,
        BlockValidationError, CommitChanges, ExecutableTx, GasOutput, InternalBlockExecutionError,
        StateDB,
    },
    eth::{dao_fork, eip6110, EthBlockExecutor, EthTxResult},
    revm::context::Block as _,
    Database, RecoveredTx,
};
#[cfg(any(test, feature = "test-hooks"))]
use alloy_primitives::keccak256;
use alloy_primitives::{map::AddressMap, Address, Bytes, Log, B256, U256};
use outbe_primitives::zero_fee_types::{BootstrapTransactionView, ZeroFeeTransaction};
use outbe_primitives::{
    block::{BlockContext, BlockHeightRegime, BlockRuntimeContext},
    consensus::ConsensusExecutionBridge,
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::PrecompileError,
    payload::validate_outbe_withdrawals,
    reshare_artifact::{
        decode_outbe_block_artifacts, ConsensusHeaderArtifact, ExecutionSummaryArtifact,
        OutbeBlockArtifacts,
    },
    storage::{direct::DirectStorageProvider, StorageHandle},
    OutbeHeader,
};
use reth_ethereum::{
    evm::{primitives::Evm, revm::context::TxEnv, RethReceiptBuilder},
    provider::BlockExecutionResult,
    Receipt, TransactionSigned,
};
use reth_evm::execute::WithTxEnv;
use reth_primitives_traits::Recovered;
use revm::context::result::{ExecutionResult, HaltReason, InvalidTransaction, OutOfGasError};
use revm::state::Account;
use std::sync::Arc;

use crate::{
    factory::OutbeEvm,
    signer::SharedOutbeEvmSigner,
    system_tx::{build_unsigned_system_tx, is_reserved_system_tx, SystemTx},
};
use reth_ethereum::chainspec::{ChainSpec, EthereumHardfork, EthereumHardforks};
use revm::database::DatabaseCommitExt;

mod artifacts;
mod block;
mod boundary;
pub mod context;
mod end_block;
mod fee_route;
mod finalization;
mod inputs;
mod post_execution;
mod pre_execution;
mod receipts;
mod system_execution;
mod system_tx_context;
#[cfg(any(test, feature = "test-hooks"))]
mod test_hooks;
pub mod witness;
mod zero_fee;

pub use system_tx_context::{
    current_preloaded_system_tx_context, record_observed_runtime_block_context,
    with_preloaded_system_tx_context, PreloadedSystemTxContext,
};
#[cfg(any(test, feature = "test-hooks"))]
pub use system_tx_context::{
    forced_dirty_system_precompile_revert, take_observed_runtime_block_contexts,
    with_forced_dirty_system_precompile_revert,
};

pub use crate::finalization::{
    validate_finalized_metadata, AccountedParentArtifact, AccountedParentArtifactProvider,
};
pub use crate::genesis::validate_genesis_state;

pub use block::OutbeBlockExecutor;
pub use inputs::{
    BlockExecutionDependencies, BlockExecutionIdentity, BlockExecutorInputs, BlockSystemPlan,
    ParentAccountingInputs,
};

#[cfg(any(test, feature = "test-hooks", feature = "test-utils"))]
pub use boundary::apply_boundary_outcome;
pub use boundary::prepare_boundary_epoch_counters;

pub use system_execution::execute::system_tx_failure_code_for_result;
pub use system_execution::layout::ResolvedBeginSystemTx;

pub use zero_fee::{
    enclave_witness_budget_deferral, is_enclave_witness_budget_deferral,
    is_sponsored_block_budget_deferral, sponsored_block_budget_deferral,
    ENCLAVE_WITNESS_BUDGET_EXCEEDED, SPONSORED_BLOCK_BUDGET_EXCEEDED,
};
pub use zero_fee::{ZeroFeeCfgAccess, ZeroFeeCfgSnapshot};

#[cfg(any(test, feature = "test-hooks"))]
pub use test_hooks::{with_phase1_verify_disabled, PHASE1_VERIFY_DISABLED};

use context::build_block_context;

#[cfg(any(test, feature = "test-hooks"))]
pub use pre_execution::run_atomic_storage_hooks;

use receipts::{burned_fee_for_gas, validator_fee_for_gas};

use zero_fee::zero_fee_transaction;

#[cfg(any(test, feature = "test-hooks"))]
pub use boundary::hash_boundary_active_set;

pub use receipts::SystemFailureReceiptInput;
