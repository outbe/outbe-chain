//! Unit tests for the CrosschainGov precompile.
//!
//! The fixtures come from `outbe_hyperlanecontroller::test_support`.
//! Cross-contract interaction goes through `HashMapStorageProvider`'s
//! sub-call stubs.

mod actions;
mod layout;
mod precompile;
mod vote_target;

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::sol_ext::IInterchainAccountRouter;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

/// Runs `data` through the precompile with zero value.
fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
) -> Result<Bytes, PrecompileError> {
    crate::precompile::dispatch(storage, data, caller, U256::ZERO)
}

/// The state-changing sub-calls after the first `start` sub-calls, as
/// `(target, calldata)`.
fn sub_calls_from(p: &HashMapStorageProvider, start: usize) -> Vec<(Address, Bytes)> {
    p.sub_call_inputs()[start..]
        .iter()
        .filter(|input| !input.is_static)
        .map(|input| (input.target, input.calldata.clone()))
        .collect()
}

/// The destination and the `(to, data)` calls of a `callRemote` calldata.
fn remote_calls(calldata: &Bytes) -> (u32, Vec<(Address, Bytes)>) {
    let call = IInterchainAccountRouter::callRemoteCall::abi_decode(calldata).unwrap();
    let calls = call
        ._calls
        .into_iter()
        .map(|c| (Address::from_word(c.to), c.data))
        .collect();
    (call._destination, calls)
}
