use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::dispatch::{
    dispatch_call, mutate, mutate_void, mutate_void_payable, reject_value_unless_payable, view,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::{validator_domain_key, HyperlaneControllerContract};

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
                    controller.initialize(sender, c.icaRouter, &c.domains, &c.isms, &c.hooks)
                }),
                fund(c) => {
                    mutate_void_payable(c, PAYABLE_SELECTORS, caller, value, |sender, _, amount| {
                        controller.fund(sender, amount)
                    })
                }
                sync(c) => mutate(c, caller, |_, _| controller.sync()),
                setHyperlaneSigner(c) => mutate_void(c, caller, |sender, c| {
                    controller.set_hyperlane_signer(sender, c.signer)
                }),
                submitCheckpoint(c) => mutate_void(c, caller, |sender, c| {
                    controller.submit_checkpoint(
                        sender,
                        c.domain,
                        c.root,
                        c.index,
                        c.messageId,
                        &c.signature,
                    )
                }),
                icaRouter(c) => view(c, |_| controller.ica_router.read()),
                ismByDomain(c) => view(c, |c| controller.ism_by_domain.read(&c.domain)),
                hookByDomain(c) => view(c, |c| controller.hook_by_domain.read(&c.domain)),
                domains(c) => view(c, |_| controller.domains.read_all()),
                hyperlaneSigner(c) => view(c, |c| controller.signer_of.read(&c.validator)),
                submittedIndex(c) => view(c, |c| {
                    controller
                        .submitted_index
                        .read(&validator_domain_key(c.validator, c.domain))
                }),
                missCount(c) => view(c, |c| controller.miss_count.read(&c.validator)),
            }
        },
    )
}
