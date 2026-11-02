use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolInterface};
use outbe_primitives::dispatch::{
    dispatch_call, mutate, mutate_void, mutate_void_payable, reject_value_unless_payable, view,
    PayableCallContext,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use outbe_hyperlanecontroller::precompile::ICrosschainGov;
use outbe_hyperlanecontroller::schema::validator_domain_key;
use outbe_hyperlanecontroller::{
    ControllerBootstrap, HyperlaneControllerContract, SignedCheckpoint,
};

use crate::executor::Executor;
use crate::schema::CrosschainGovContract;

/// Selectors that accept native value: only `fund`, the top-up of the balance
/// that pays Interchain Account dispatch fees. The route table binds this list
/// to the address's `ValuePolicy` at compile time.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[ICrosschainGov::fundCall::SELECTOR];

/// Dispatch for the precompile at the owner address. The Hyperlane selectors
/// go to `hyperlanecontroller` with this crate as the owner.
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
            let mut controller = HyperlaneControllerContract::new(storage.clone());
            let mut gov = CrosschainGovContract::new(storage.clone());
            let mut executor = Executor::new(storage.clone());
            match call {
                initialize(c) => mutate_void(&storage, c, caller, |sender, c| {
                    storage.with_checkpoint(|| {
                        let calls = controller.initialize(
                            sender,
                            &ControllerBootstrap {
                                ica_router: c.icaRouter,
                                validator_announce: c.validatorAnnounce,
                                domains: &c.domains,
                                isms: &c.isms,
                                hooks: &c.hooks,
                            },
                        )?;
                        executor.run(calls)
                    })
                }),
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
                sync(c) => mutate(&storage, c, caller, |_, _| {
                    storage.with_checkpoint(|| match controller.sync()? {
                        Some(calls) => executor.run(calls).map(|()| true),
                        None => Ok(false),
                    })
                }),
                applyApproved(c) => mutate_void(&storage, c, caller, |_, c| {
                    gov.apply_approved(c.proposalId, &c.payload)
                }),
                setHyperlaneSigner(c) => mutate_void(&storage, c, caller, |sender, c| {
                    controller.set_hyperlane_signer(sender, c.signer)
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
                remoteAccount(c) => view(c, |c| executor.remote_account(c.domain)),
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
