pub mod begin_block_precompile;
pub mod builder;
pub mod config;
mod create_guard;
pub mod executor;
pub mod factory;
pub mod failure_receipt;
pub mod gas;
pub mod handlers;
mod native_delegation;
pub(crate) mod precompile_routes;
pub mod precompiles;
pub mod storage;
pub mod sub_call;
pub mod tee_attestation_activation;
pub mod zk;
#[cfg(test)]
mod zk_tests;
/// Re-export of the validator EVM signer, which now lives in
/// `outbe-primitives::signer`. It is a wire/data-only type (no EVM runtime), so it
/// belongs with the other primitives. The `outbe_evm::signer` path stays, so the
/// existing `pub use` re-exports below continue to work.
pub use outbe_primitives::signer;
/// Re-export of the system-tx codec, which now physically lives in
/// `outbe-primitives::system_tx`. The codec is wire/data-only (no EVM
/// runtime), so it belongs with the rest of the consensus primitives.
/// The path `outbe_evm::system_tx` continues to work, so ~50 call sites in
/// executor / payload builder / tests need no change.
pub use outbe_primitives::system_tx;

pub use config::{
    OutbeBlockAssembler, OutbeBlockExecutionCtx, OutbeEvmConfig, OutbeExecutorBuilder,
    OutbeNextBlockEnvAttributes, RethAccountedParentArtifactProvider,
};
pub use executor::{AccountedParentArtifact, AccountedParentArtifactProvider};
pub use factory::OutbeEvmFactory;
pub use signer::{
    default_validator_evm_key_path, OutbeEvmSigner, SharedOutbeEvmSigner, SignerError,
};
