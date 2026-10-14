//! Snapshot context for background enclave calls (DKG, renewal, health, keys).
use crate::errors::{CallContextError, TeeCallContextError};
use alloy_consensus::BlockHeader as _;
use alloy_primitives::{B256, U256};
use outbe_primitives::{addresses::UPDATE_ADDRESS, OutbeHeader};
use outbe_tee::call_context::{EnclaveCallContextV1, EnclaveContextKindV1};
use reth_provider::{BlockIdReader, HeaderProvider, StateProviderFactory};
use reth_storage_api::StateProvider;

pub fn read<P>(
    provider: &P,
    chain_id: u64,
    genesis_hash: B256,
) -> Result<EnclaveCallContextV1, CallContextError>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    read_typed(provider, chain_id, genesis_hash)
        .map_err(TeeCallContextError::into_call_context_error)
}

pub fn read_typed<P>(
    provider: &P,
    chain_id: u64,
    genesis_hash: B256,
) -> Result<EnclaveCallContextV1, TeeCallContextError>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let hash = provider
        .finalized_block_num_hash()?
        .map(|block| block.hash)
        .unwrap_or(genesis_hash);
    let header = provider
        .sealed_header_by_hash(hash)?
        .ok_or(TeeCallContextError::HeaderUnavailable)?;
    let state = provider.state_by_block_hash(hash)?;
    let version = state
        .storage(UPDATE_ADDRESS, B256::ZERO)?
        .unwrap_or_default();
    if version > U256::from(u32::MAX) {
        return Err(TeeCallContextError::ProtocolVersionOverflow);
    }
    Ok(EnclaveCallContextV1 {
        kind: EnclaveContextKindV1::Snapshot,
        chain_id,
        genesis_hash,
        block_number: header.number(),
        block_timestamp: header.timestamp(),
        protocol_version: version.to(),
    })
}
