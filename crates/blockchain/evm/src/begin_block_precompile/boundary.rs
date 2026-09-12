use crate::executor::apply_boundary_outcome;
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
    Ok(())
}
