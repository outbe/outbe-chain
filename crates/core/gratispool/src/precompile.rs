//! Gratispool precompile at `0x2004`. Diagnostics-only ABI — view methods
//! and event signatures. State-changing entry points (`pledgeGratis`,
//! `unpledgeGratis`, `requestCredis`, `anadosis`) live on the gratisfactory
//! and credisfactory precompiles and reach the pool through the Rust
//! cross-module API (see [`crate::api`]).

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_primitives::addresses::GRATIS_POOL_ADDRESS;
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::constants::DenomAmount;
use crate::schema::GratisPoolContract;

sol!("../../../contracts/precompiles/src/IGratisPool.sol");

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    let _ = caller;
    dispatch_call(data, IGratisPool::IGratisPoolCalls::abi_decode, |call| {
        use IGratisPool::IGratisPoolCalls::*;
        match call {
            currentRoot(c) => view(c, |c| {
                let pool = GratisPoolContract::new(storage.clone());
                pool.current_root(c.denomId)
            }),
            leafCount(c) => view(c, |c| {
                let pool = GratisPoolContract::new(storage.clone());
                pool.leaf_count(c.denomId)
            }),
            isSpent(c) => view(c, |c| {
                let pool = GratisPoolContract::new(storage.clone());
                pool.nullifier_spent.contains(&c.nullifierHash)
            }),
            supportedDenoms(_) => metadata::<IGratisPool::supportedDenomsCall>(|| {
                Ok(IGratisPool::supportedDenomsReturn {
                    ids: DenomAmount::ALL.iter().map(|d| d.id()).collect(),
                    amounts: DenomAmount::ALL.iter().map(|d| d.amount()).collect(),
                })
            }),
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID)
            }),
        }
    })
}

/// Helper that lets the gratispool runtime emit `CommitmentInserted` from a
/// non-ABI path (the Rust `api::add_commitment` / `api::insert_reclaim`
/// entrypoints). Kept here so the Solidity-ABI event encoding stays
/// alongside the rest of the `IGratisPool` glue.
pub(crate) fn emit_commitment_inserted(
    storage: &StorageHandle<'_>,
    denom_id: u8,
    commitment: U256,
    leaf_index: u32,
    new_root: U256,
) -> Result<()> {
    let event = IGratisPool::CommitmentInserted {
        denomId: denom_id,
        commitment,
        leafIndex: leaf_index,
        newRoot: new_root,
    };
    storage.emit_event(
        GRATIS_POOL_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&event),
    )
}

/// Helper for orchestrators to emit `NullifierSpent` after a successful
/// `verify_and_spend_*`. Exposed because the orchestration layer
/// (gratisfactory / credisfactory) owns the user-facing dispatch and the
/// matching log; the pool only owns the cryptographic state.
pub fn emit_nullifier_spent(
    storage: &StorageHandle<'_>,
    nullifier: U256,
    action_tag: u8,
) -> Result<()> {
    let event = IGratisPool::NullifierSpent {
        nullifierHash: nullifier,
        action: action_tag,
    };
    storage.emit_event(
        GRATIS_POOL_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&event),
    )
}
