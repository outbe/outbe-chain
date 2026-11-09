use alloy_primitives::address;
use alloy_primitives::keccak256;
use alloy_primitives::Address;
use alloy_primitives::Bytes;
use alloy_primitives::B256;
use alloy_primitives::U256;
use outbe_evm::executor::AccountedParentArtifact;
use outbe_evm::sys_tx_precompile::{with_preloaded_system_tx_context, PreloadedSystemTxContext};
use outbe_evm::system_tx::SystemTx;
use outbe_evm::EvmRuntimeHost;
use outbe_primitives::addresses::SYSTEM_ADDRESS;
use outbe_primitives::block::BlockContext;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::consensus::DkgBoundaryArtifact;
use outbe_primitives::consensus::ReshareResult;
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::error::Result;
use outbe_primitives::reshare_artifact::LateFinalizeCreditsArtifact;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

mod fixtures;
use fixtures::{
    boundary_noop, configured_storage, metadata, provider_from_storage, runtime_ctx, CHAIN_ID,
    OWNER, VALIDATOR,
};

mod dispatch;

mod finalization;

mod lease_and_boundary;

mod tee_bootstrap;

mod late_credits;
mod nod_cleanup;
mod publication;

fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: alloy_primitives::Address,
    value: alloy_primitives::U256,
) -> outbe_primitives::error::Result<alloy_primitives::Bytes> {
    outbe_runtime::OutbeRuntime::new().dispatch_system_tx(
        storage,
        data,
        caller,
        value,
        &outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1::Unbound,
    )
}
