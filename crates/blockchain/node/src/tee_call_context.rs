//! Snapshot context for background enclave calls (DKG, renewal, health, keys).
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
) -> Result<EnclaveCallContextV1, String>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    let hash = provider
        .finalized_block_num_hash()
        .map_err(|e| e.to_string())?
        .map(|block| block.hash)
        .unwrap_or(genesis_hash);
    let header = provider
        .sealed_header_by_hash(hash)
        .map_err(|e| e.to_string())?
        .ok_or("enclave context header unavailable")?;
    let state = provider
        .state_by_block_hash(hash)
        .map_err(|e| e.to_string())?;
    let version = state
        .storage(UPDATE_ADDRESS, B256::ZERO)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    if version > U256::from(u32::MAX) {
        return Err("enclave context protocol version overflow".into());
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
