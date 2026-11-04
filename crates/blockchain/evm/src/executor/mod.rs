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
use alloy_primitives::{keccak256, map::AddressMap, Address, Bytes, Log, B256, U256};
use outbe_primitives::{
    block::{BlockContext, BlockHeightRegime, BlockLifecycle, BlockRuntimeContext},
    consensus::{ConsensusExecutionBridge, GenesisValidators},
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::PrecompileError,
    hook_events::partition_hook_events,
    payload::validate_outbe_withdrawals,
    reshare_artifact::{
        decode_outbe_block_artifacts, ConsensusHeaderArtifact, ExecutionSummaryArtifact,
        OutbeBlockArtifacts,
    },
    storage::{direct::DirectStorageProvider, StorageHandle},
    OutbeHeader,
};
use outbe_zerofee::{BootstrapTransactionView, ZeroFeeTransaction};
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
    sys_tx_precompile::{with_preloaded_system_tx_context, PreloadedSystemTxContext},
    factory::OutbeEvm,
    signer::SharedOutbeEvmSigner,
    system_tx::{
        build_unsigned_system_tx_with_gas_limit, is_reserved_system_tx, SystemTx,
        SystemTxVisibleGasPlan,
    },
};
use reth_ethereum::chainspec::{ChainSpec, EthereumHardfork, EthereumHardforks};
use revm::database::DatabaseCommitExt;

mod artifacts;
mod block;
mod boundary;
pub(crate) mod context;
mod end_block;
mod fee_route;
mod finalization;
mod hooks;
mod inputs;
mod post_execution;
mod pre_execution;
mod receipts;
mod system_execution;
#[cfg(test)]
mod tests;
pub(crate) mod witness;
mod zero_fee;

pub use crate::finalization::{
    validate_finalized_metadata, AccountedParentArtifact, AccountedParentArtifactProvider,
};
pub use crate::genesis::validate_genesis_state;

pub use block::OutbeBlockExecutor;
pub(crate) use inputs::{
    BlockExecutionDependencies, BlockExecutionIdentity, BlockExecutorInputs, BlockSystemPlan,
    ParentAccountingInputs,
};

pub use boundary::marker_addresses;

pub use hooks::run_outbe_pre_execution_hooks;

pub(crate) use boundary::{apply_boundary_outcome, prepare_boundary_epoch_counters};

pub(crate) use system_execution::execute::system_tx_failure_code_for_result;
pub(crate) use system_execution::layout::ResolvedBeginSystemTx;

pub use zero_fee::{
    enclave_witness_budget_deferral, is_enclave_witness_budget_deferral,
    is_sponsored_block_budget_deferral, sponsored_block_budget_deferral,
    ENCLAVE_WITNESS_BUDGET_EXCEEDED, SPONSORED_BLOCK_BUDGET_EXCEEDED,
};
pub(crate) use zero_fee::{ZeroFeeCfgAccess, ZeroFeeCfgSnapshot};

#[cfg(test)]
pub(crate) use tests::harness::with_phase1_verify_disabled;

use context::build_block_context;

#[cfg(test)]
use hooks::run_atomic_storage_hooks;

use receipts::{burned_fee_for_gas, validator_fee_for_gas, SystemFailureReceiptInput};

use zero_fee::zero_fee_transaction;

#[cfg(test)]
use boundary::hash_boundary_active_set;

#[cfg(test)]
pub(crate) use tests::harness::PHASE1_VERIFY_DISABLED;
