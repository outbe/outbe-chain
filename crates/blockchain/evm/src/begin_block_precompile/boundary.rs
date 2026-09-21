use crate::executor::apply_boundary_outcome;
use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::precompile::IHyperlaneController;
use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::consensus::DkgBoundaryArtifact;
use outbe_primitives::error::Result;

/// BoundaryOutcome system tx: activate a DKG/reshare boundary before user transactions.
pub(crate) fn run_boundary_outcome(
    ctx: &BlockRuntimeContext,
    artifact: &DkgBoundaryArtifact,
) -> Result<()> {
    let was_participant = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone())
        .is_consensus_participant(ctx.block.proposer)?;
    apply_boundary_outcome(
        ctx.storage.clone(),
        artifact,
        ctx.block.block_number,
        ctx.block.timestamp,
    )?;
    if !was_participant {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
        if vs.is_consensus_participant(ctx.block.proposer)? {
            vs.record_proposer(ctx.block.proposer)?;
        }
    }
    // Record the TEE recipient X25519 pubkeys announced through this boundary
    // (the `BoundaryOutcome` key-delivery channel - README "Consensus Artifact
    // Transport"). These ride in `header.extra_data` and are part of the
    // hash-committed `OutbeBlockArtifacts`, so every validator records the same
    // ordered set deterministically. Empty for boundaries that announce none.
    if !artifact.tee_recipient_pubkeys.is_empty() {
        outbe_teeregistry::TeeRegistry::new(ctx.storage.clone())
            .record_boundary_recipient_keys(&artifact.tee_recipient_pubkeys)?;
    }
    sync_hyperlane_validators(ctx);
    Ok(())
}

/// Mirrors the just-activated validator set into the Hyperlane ISMs by
/// sub-calling the controller's `sync()`. A child frame so the controller,
/// not the system tx, is the router's `msg.sender` (its Interchain Account
/// derives from that). Best effort: a bridge failure (uninitialized
/// controller, unfunded fee) must never block epoch activation, so the error
/// is logged and the next boundary or a manual `sync()` retries.
fn sync_hyperlane_validators(ctx: &BlockRuntimeContext) {
    let call = IHyperlaneController::syncCall {}.abi_encode().into();
    if let Err(error) = ctx
        .storage
        .call(HYPERLANE_CONTROLLER_ADDRESS, U256::ZERO, call)
    {
        tracing::warn!(
            target: "outbe::hyperlane",
            block = ctx.block.block_number,
            %error,
            "hyperlane validator sync skipped at epoch boundary"
        );
    }
}
