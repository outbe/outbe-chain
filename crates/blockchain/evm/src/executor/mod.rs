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
use outbe_compressed_entities::ExecutionScope;
use outbe_ocomp_protocol::system_carrier::{
    classify_ocomp_system_carrier, OcompSystemCarrierCandidate, OcompSystemCarrierView,
    OCOMP_SYSTEM_CARRIER_INTERNAL_GAS_LIMIT,
};
use outbe_offchain_data::{ExecutionReadBudgetGuard, RuntimeBodyReaders};
use outbe_primitives::{
    block::{BlockContext, BlockLifecycle, BlockRuntimeContext},
    consensus::{ConsensusExecutionBridge, GenesisValidators},
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::{PrecompileError, Result as OutbeResult},
    hook_events::partition_hook_events,
    payload::validate_outbe_withdrawals,
    reshare_artifact::{
        decode_outbe_block_artifacts, CompressedEntitiesRootArtifact, ConsensusHeaderArtifact,
        ExecutionSummaryArtifact,
    },
    storage::{direct::DirectStorageProvider, StorageHandle},
    OutbeHeader,
};
use outbe_validatorset::ValidatorLifecycle;
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
use std::{collections::BTreeSet, sync::Arc};

use crate::{
    begin_block_precompile::{with_preloaded_system_tx_context, PreloadedSystemTxContext},
    factory::OutbeEvm,
    signer::SharedOutbeEvmSigner,
    system_tx::{
        build_unsigned_system_tx, build_unsigned_system_tx_with_gas_limit,
        expected_begin_block_kinds_for_activation, is_reserved_system_tx,
        validate_phase1_witness_against, OcompLifecycleActivation, SystemTxInputV2, SystemTxKind,
        SystemTxVisibleGasPlan,
    },
};
use reth_ethereum::chainspec::{ChainSpec, EthereumHardfork, EthereumHardforks};
use revm::database::DatabaseCommitExt;

mod accounting;
mod artifacts;
mod block;
mod boundary;
mod compressed_entities;
mod context;
mod hooks;
mod late_finalize;
mod receipts;
mod system_execution;
#[cfg(test)]
mod tests;
mod zero_fee;

pub use accounting::{AccountedParentArtifact, AccountedParentArtifactProvider};

pub use block::OutbeBlockExecutor;

pub use boundary::marker_addresses;

pub use hooks::{run_outbe_pre_execution_hooks, run_outbe_pre_execution_hooks_with_readers};

pub(crate) use accounting::validate_finalized_metadata;

pub(crate) use boundary::{apply_boundary_outcome, prepare_boundary_epoch_counters};

pub(crate) use system_execution::execute::system_tx_failure_code_for_result;

pub(crate) use zero_fee::{ZeroFeeCfgAccess, ZeroFeeCfgSnapshot};

#[cfg(test)]
pub(crate) use tests::harness::with_phase1_verify_disabled;

use artifacts::validate_execution_summary_artifact;

use compressed_entities::{
    validate_compressed_entities_root_after_seal, validate_compressed_entities_root_scheme,
};

use context::{build_block_context, validate_genesis_state};

use hooks::{run_atomic_storage_hook_with_output, run_atomic_storage_hooks};

use receipts::{validator_fee_for_gas, SystemFailureReceiptInput};

use system_execution::execute::{
    is_nod_materialization_soft_revert, is_ocomp_deadline_passed_revert,
};

use zero_fee::{bootstrap_transaction, zero_fee_transaction};

#[cfg(test)]
use boundary::hash_boundary_active_set;

#[cfg(test)]
use tests::harness::PHASE1_VERIFY_DISABLED;

#[cfg(test)]
pub(crate) use hooks::enforce_enclave_upgrade_deadline;
