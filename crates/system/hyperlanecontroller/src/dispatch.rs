//! The Hyperlane selectors of the precompile at `0x…EE14`.
//!
//! The CrosschainGov precompile decodes the call and sends every Hyperlane
//! selector here, with itself as the [`Owner`].

use alloy_primitives::{Address, Bytes};
use outbe_primitives::dispatch::{mutate, mutate_void, view};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use crate::abi::ICrosschainGov::ICrosschainGovCalls;
use crate::schema::{validator_domain_key, HyperlaneControllerContract};
use crate::{ControllerBootstrap, Owner, SignedCheckpoint};

/// Runs one Hyperlane selector. A selector of the CrosschainGov precompile
/// itself (`applyApproved`, `fund`, `remoteAccount`) reverts.
pub fn dispatch(
    storage: StorageHandle<'_>,
    call: ICrosschainGovCalls,
    caller: Address,
    owner: &mut impl Owner,
) -> Result<Bytes> {
    use ICrosschainGovCalls::*;
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
                owner,
            )
        }),
        sync(c) => mutate(&storage, c, caller, |_, _| controller.sync(owner)),
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
        applyApproved(_) | fund(_) | remoteAccount(_) => Err(PrecompileError::Revert(
            "not a Hyperlane controller selector".to_string(),
        )),
    }
}
