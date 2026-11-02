//! Vote actions on the Hyperlane chains. They call the methods of
//! `hyperlanecontroller` with the owner as the executor.

use alloy_primitives::Address;
use outbe_hyperlanecontroller::HyperlaneControllerContract;
use outbe_primitives::error::Result;

use crate::executor::Executor;

/// `addHyperlaneChain`: the Outbe ICA router enrolls `ica_router`, then the
/// validator sync reaches `chain_id`.
pub(super) fn add_chain(
    controller: &mut HyperlaneControllerContract<'_>,
    executor: &mut Executor<'_>,
    chain_id: u32,
    ica_router: Address,
    ism: Address,
    hook: Address,
) -> Result<()> {
    controller.enroll_ica_router(executor, chain_id, ica_router)?;
    controller.add_domain(executor, chain_id, ism, hook)
}
