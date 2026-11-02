//! Owner calls through the CrosschainGov precompile.
//!
//! The controller owns no contract. It writes to an ISM or to the ICA router
//! by calling `CROSSCHAIN_GOV_ADDRESS` like a contract: `callLocal` for a call
//! on Outbe, `callRemote` for a call on another chain. The owner checks that
//! the caller is in its owner-caller list.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::addresses::CROSSCHAIN_GOV_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::sol_ext::ICrosschainGov;

/// A zero-value call of `to`.
pub(crate) fn call(to: Address, data: impl Into<Bytes>) -> ICrosschainGov::Call {
    ICrosschainGov::Call {
        to,
        value: U256::ZERO,
        data: data.into(),
    }
}

/// The owner calls `call.to` on Outbe.
pub(crate) fn call_local(storage: &StorageHandle, call: ICrosschainGov::Call) -> Result<()> {
    owner_call(storage, ICrosschainGov::callLocalCall { call }.abi_encode())
}

/// The owner runs `calls` on `domain` through its Interchain Account.
pub(crate) fn call_remote(
    storage: &StorageHandle,
    domain: u32,
    calls: Vec<ICrosschainGov::Call>,
) -> Result<()> {
    owner_call(
        storage,
        ICrosschainGov::callRemoteCall { domain, calls }.abi_encode(),
    )
}

fn owner_call(storage: &StorageHandle, data: Vec<u8>) -> Result<()> {
    storage
        .call(CROSSCHAIN_GOV_ADDRESS, U256::ZERO, data.into())
        .map(|_| ())
}
