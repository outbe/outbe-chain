use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::dispatch::{
    dispatch_call, mutate, mutate_void, mutate_void_payable, reject_value_unless_payable, view,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::HyperlaneControllerContract;

/// Selectors that accept native value: only `fund`, the top-up of the balance
/// that pays Interchain Account dispatch fees. The route table binds this list
/// to the address's `ValuePolicy` at compile time.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[IHyperlaneController::fundCall::SELECTOR];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IHyperlaneController.sol"
);

/// Dispatch for the Hyperlane controller precompile.
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    dispatch_call(
        data,
        IHyperlaneController::IHyperlaneControllerCalls::abi_decode,
        |call| {
            use IHyperlaneController::IHyperlaneControllerCalls::*;
            let mut controller = HyperlaneControllerContract::new(storage);
            match call {
                initialize(c) => mutate_void(c, caller, |sender, c| {
                    controller.initialize(sender, c.router, &c.domains, &c.isms)
                }),
                fund(c) => {
                    mutate_void_payable(c, PAYABLE_SELECTORS, caller, value, |sender, _, amount| {
                        controller.fund(sender, amount)
                    })
                }
                sync(c) => mutate(c, caller, |_, _| controller.sync()),
                router(c) => view(c, |_| controller.router.read()),
                ismByDomain(c) => view(c, |c| controller.ism_by_domain.read(&c.domain)),
                domains(c) => view(c, |_| controller.domains.read_all()),
            }
        },
    )
}
