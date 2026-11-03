//! Dispatch for the HyperlaneController precompile.
//!
//! `initialize` and `sync` make owner calls through `CROSSCHAIN_BRIDGE_ADDRESS`
//! (`callLocal`, `callRemote`). The owner then is the `msg.sender` of every
//! ISM and router call.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void, view};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::{validator_domain_key, HyperlaneControllerContract};
use crate::{ControllerBootstrap, SignedCheckpoint};

/// No selector accepts native value. The owner holds the fee balance.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IHyperlaneController.sol"
);

/// Dispatch for the HyperlaneController precompile.
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    _value: U256,
) -> Result<Bytes> {
    dispatch_call(
        data,
        IHyperlaneController::IHyperlaneControllerCalls::abi_decode,
        |call| {
            use IHyperlaneController::IHyperlaneControllerCalls::*;
            let mut controller = HyperlaneControllerContract::new(storage.clone());
            match call {
                initialize(c) => mutate_void(&storage, c, caller, |sender, c| {
                    controller.initialize(
                        sender,
                        &ControllerBootstrap {
                            ica_router: c.icaRouter,
                            validator_announce: c.validatorAnnounce,
                            domains: &c.domains,
                            isms: &c.isms,
                            hooks: &c.hooks,
                        },
                    )
                }),
                sync(c) => mutate(&storage, c, caller, |_, _| controller.sync()),
                setHyperlaneSigner(c) => mutate_void(&storage, c, caller, |sender, c| {
                    controller.set_hyperlane_signer(sender, c.signer)
                }),
                applyApproved(c) => mutate_void(&storage, c, caller, |_, c| {
                    controller.apply_approved(c.proposalId, &c.payload)
                }),
                submitCheckpoint(c) => mutate_void(&storage, c, caller, |sender, c| {
                    controller.submit_checkpoint(
                        sender,
                        &SignedCheckpoint {
                            domain: c.domain,
                            root: c.root,
                            index: c.index,
                            message_id: c.messageId,
                            signature: &c.signature,
                        },
                    )
                }),
                icaRouter(c) => view(c, |_| controller.ica_router.read()),
                validatorAnnounce(c) => view(c, |_| controller.validator_announce.read()),
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
