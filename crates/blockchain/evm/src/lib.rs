pub mod builder;
pub mod config;
mod create_guard;
pub mod errors;
pub mod executor;
pub mod factory;
pub mod failure_receipt;
pub mod finalization;
pub mod gas;
pub mod genesis;
mod native_delegation;
pub mod precompiles;
pub mod storage;
pub mod sub_call;
pub use outbe_tee::tee_attestation_activation;
pub mod tee_bootstrap;
pub mod test_support;

/// Re-export of the validator EVM signer, which now lives in
/// `outbe-primitives::signer`. It is a wire/data-only type (no EVM runtime), so it
/// belongs with the other primitives. The `outbe_evm::signer` path stays, so the
/// existing `pub use` re-exports below continue to work.
pub use outbe_primitives::signer;
/// Execution-layer system-transaction schedule, FSM, prefix assembler, and
/// layout validation (`BlockOrder`, `ProtocolStep`, `BlockSystemPlan`,
/// `BlockExecutionFsm`, `assemble_prefix_system_txs`, `validate_system_layout`),
/// plus re-exports of the wire codec from `outbe_primitives::system_tx`.
pub mod system_tx;

pub use config::{
    OutbeBlockAssembler, OutbeBlockExecutionCtx, OutbeEvmConfig, OutbeExecutorBuilder,
    OutbeNextBlockEnvAttributes, RethAccountedParentArtifactProvider,
};
pub use executor::{
    enclave_witness_budget_deferral, is_enclave_witness_budget_deferral,
    is_sponsored_block_budget_deferral, sponsored_block_budget_deferral,
    ENCLAVE_WITNESS_BUDGET_EXCEEDED, SPONSORED_BLOCK_BUDGET_EXCEEDED,
};
pub use executor::{AccountedParentArtifact, AccountedParentArtifactProvider};
pub use factory::OutbeEvmFactory;
pub use signer::{
    default_validator_evm_key_path, OutbeEvmSigner, SharedOutbeEvmSigner, SignerError,
};

mod host;
#[cfg(any(test, feature = "test-utils"))]
pub use host::stub_host;
pub use host::{EvmRuntimeHost, UserTxFeePolicy};
