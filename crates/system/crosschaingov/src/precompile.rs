use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolInterface};
use outbe_primitives::dispatch::{
    dispatch_call, mutate_void, mutate_void_payable, reject_value_unless_payable, view,
    PayableCallContext,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use outbe_hyperlanecontroller::abi::ICrosschainGov;

use crate::schema::CrosschainGov;

/// Selectors that accept native value: only `fund`, the top-up of the balance
/// that pays Interchain Account dispatch fees. The route table binds this list
/// to the address's `ValuePolicy` at compile time.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[ICrosschainGov::fundCall::SELECTOR];

/// Dispatch for the CrosschainGov precompile. `applyApproved`, `fund` and
/// `remoteAccount` run here. Every other selector goes to the Hyperlane
/// controller, with this precompile as its owner.
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
            let mut gov = CrosschainGov::new(storage.clone());
            match call {
                applyApproved(c) => mutate_void(&storage, c, caller, |_, c| {
                    gov.apply_approved(c.proposalId, &c.payload)
                }),
                fund(c) => mutate_void_payable(
                    &storage,
                    c,
                    PayableCallContext {
                        selectors: PAYABLE_SELECTORS,
                        sender: caller,
                        value,
                    },
                    |sender, _, amount| gov.fund(sender, amount),
                ),
                remoteAccount(c) => view(c, |c| gov.remote_account(c.domain)),
                hyperlane => outbe_hyperlanecontroller::dispatch(
                    storage.clone(),
                    hyperlane,
                    caller,
                    &mut gov,
                ),
            }
        },
    )
}
