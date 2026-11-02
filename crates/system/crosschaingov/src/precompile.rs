//! Dispatch for the CrosschainGov precompile, the owner of every multi-chain
//! contract.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::addresses::{CROSSCHAIN_GOV_ADDRESS, HYPERLANE_CONTROLLER_ADDRESS};
use outbe_primitives::dispatch::{
    dispatch_call, mutate, mutate_void, mutate_void_payable, reject_value_unless_payable, view,
    PayableCallContext,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::calls::RemoteCall;
use crate::errors::CrosschainGovError;
use crate::executor::Executor;
use crate::schema::CrosschainGovContract;

/// Precompiles that may make the owner call a contract through `callLocal` and
/// `callRemote`. The owner itself is in the list: its vote actions run
/// HyperlaneController methods, and they call back here from the owner frame.
pub const OWNER_CALLERS: &[Address] = &[HYPERLANE_CONTROLLER_ADDRESS, CROSSCHAIN_GOV_ADDRESS];

/// Selectors that accept native value: only `fund`, the top-up of the balance
/// that pays Interchain Account dispatch fees. The route table binds this list
/// to the address's `ValuePolicy` at compile time.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[ICrosschainGov::fundCall::SELECTOR];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/ICrosschainGov.sol"
);

/// Dispatch for the CrosschainGov precompile.
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    dispatch_call(
        data,
        ICrosschainGov::ICrosschainGovCalls::abi_decode,
        |call| {
            use ICrosschainGov::ICrosschainGovCalls::*;
            let mut gov = CrosschainGovContract::new(storage.clone());
            let mut executor = Executor::new(storage.clone());
            match call {
                fund(c) => mutate_void_payable(
                    &storage,
                    c,
                    PayableCallContext {
                        selectors: PAYABLE_SELECTORS,
                        sender: caller,
                        value,
                    },
                    |sender, _, amount| executor.fund(sender, amount),
                ),
                applyApproved(c) => mutate_void(&storage, c, caller, |_, c| {
                    gov.apply_approved(c.proposalId, &c.payload)
                }),
                callLocal(c) => mutate(&storage, c, caller, |sender, c| {
                    require_owner_caller(sender)?;
                    executor.call_local(c.call.to, c.call.value, c.call.data)
                }),
                callRemote(c) => mutate(&storage, c, caller, |sender, c| {
                    require_owner_caller(sender)?;
                    let calls: Vec<RemoteCall> = c
                        .calls
                        .into_iter()
                        .map(|call| RemoteCall {
                            to: call.to,
                            value: call.value,
                            data: call.data,
                        })
                        .collect();
                    executor.call_remote(c.domain, &calls)
                }),
                remoteAccount(c) => view(c, |c| executor.remote_account(c.domain)),
            }
        },
    )
}

fn require_owner_caller(caller: Address) -> Result<()> {
    if OWNER_CALLERS.contains(&caller) {
        Ok(())
    } else {
        Err(CrosschainGovError::NotOwnerCaller { caller }.into())
    }
}
