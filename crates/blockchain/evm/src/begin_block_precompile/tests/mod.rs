use super::*;
use crate::executor::AccountedParentArtifact;
use crate::system_tx::SystemTxInputV2;
use alloy_primitives::address;
use alloy_primitives::keccak256;
use alloy_primitives::Address;
use alloy_primitives::Bytes;
use alloy_primitives::B256;
use alloy_primitives::U256;
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
    GENESIS_HASH, OWNER, VALIDATOR,
};

mod dispatch;

mod finalization;

mod lease_and_boundary;

mod tee_bootstrap;

mod late_credits;
