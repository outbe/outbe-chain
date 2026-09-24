use super::super::*;

/// reth's canonical head can lead consensus finalization by the in-flight block
/// (steady state: `head_height = finalized_height + 1`; a few during a
/// finalization hiccup). On a plain restart in that window the head has no
/// finalization record yet - a normal unfinalized head, not archive corruption.
/// A head leading the marshal finalized tip by at most this many blocks is
/// treated as that benign case; a larger lead is suspicious and stays fatal.
pub(in crate::stack) const MAX_UNFINALIZED_HEAD_LEAD: u64 = 16;

/// Whether an execution head that leads the marshal's durable finalized tip is
/// the benign "unfinalized in-flight head" case rather than archive corruption:
/// a real finalized tip (`> 0`) and a positive, bounded lead. The caller still
/// confirms the marshal actually holds the finalized tip's finalization record
/// before treating the restart as recoverable.
pub(in crate::stack) fn unfinalized_head_lead_is_recoverable(
    last_execution_height: u64,
    finalized_tip: u64,
) -> bool {
    let head_lead = last_execution_height.saturating_sub(finalized_tip);
    finalized_tip > 0 && head_lead > 0 && head_lead <= MAX_UNFINALIZED_HEAD_LEAD
}

/// Highest height that both the execution store and durable consensus finality
/// can authorize at startup. An execution-only head is speculative and must not
/// seed finalized forkchoice state; a consensus-only suffix is backfilled later.
pub(in crate::stack) fn durable_recovery_anchor_height(
    last_execution_height: u64,
    finalized_tip: u64,
) -> u64 {
    last_execution_height.min(finalized_tip)
}

/// Inclusive suffix that must be authenticated before a certified follower's
/// Marshal actor can repair or dispatch durable consensus records.
pub(in crate::stack) fn certified_follower_replay_suffix_bounds(
    archive_finalization_tip: u64,
    archive_block_tip: u64,
    execution_tip: u64,
) -> (u64, u64) {
    (
        archive_finalization_tip
            .min(archive_block_tip)
            .min(execution_tip),
        archive_finalization_tip.max(archive_block_tip),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::stack) struct CertifiedFollowerRecoveryFloors {
    pub(in crate::stack) marshal_processed: u64,
    pub(in crate::stack) archive_finalization_tip: u64,
    pub(in crate::stack) archive_block_tip: u64,
    pub(in crate::stack) execution_tip: u64,
    pub(in crate::stack) reth_finalized: u64,
}

/// Select the only height that can still become the certified follower startup
/// anchor before exact identity and certificate verification. Heights merely
/// bound the candidate: they never authorize it without the later hash and
/// certificate checks.
pub(in crate::stack) fn select_certified_follower_recovery_height(
    floors: CertifiedFollowerRecoveryFloors,
) -> Result<u64> {
    let anchor = floors
        .archive_finalization_tip
        .min(floors.archive_block_tip)
        .min(floors.execution_tip);
    ensure!(
        floors.marshal_processed <= anchor,
        "Marshal processed floor {} exceeds certified follower recovery anchor {anchor}",
        floors.marshal_processed,
    );
    ensure!(
        anchor <= floors.marshal_processed.saturating_add(1),
        "certified follower recovery anchor {anchor} exceeds Marshal processed floor {} by more than one block",
        floors.marshal_processed,
    );
    ensure!(
        floors.reth_finalized <= anchor,
        "Reth finalized height {} exceeds recovery anchor {anchor}",
        floors.reth_finalized,
    );
    Ok(anchor)
}

#[derive(Clone, Debug)]
pub(in crate::stack) struct CertifiedFollowerRecoveryAnchor {
    pub(in crate::stack) checkpoint: ProjectionCheckpoint,
    pub(in crate::stack) finalization: Option<outbe_consensus::marshal_types::Finalization>,
    pub(in crate::stack) block: outbe_consensus::block::ConsensusBlock,
}

#[allow(clippy::too_many_arguments)]
pub(in crate::stack) fn validate_certified_follower_recovery_record(
    height: u64,
    canonical_hash: B256,
    local_finalization: &outbe_consensus::marshal_types::Finalization,
    local_block: &outbe_consensus::block::ConsensusBlock,
    upstream_finalization: &outbe_consensus::marshal_types::Finalization,
    upstream_block: &outbe_consensus::block::ConsensusBlock,
    schemes: &HybridSchemeProvider<MinSig>,
) -> Result<CertifiedFollowerRecoveryAnchor> {
    ensure!(
        local_block.number() == height,
        "local archived block reports height {}, expected {height}",
        local_block.number(),
    );
    ensure!(
        upstream_block.number() == height,
        "upstream certified block reports height {}, expected {height}",
        upstream_block.number(),
    );
    ensure!(
        local_block.block_hash() == canonical_hash,
        "local archived block hash {} differs from canonical Reth hash {canonical_hash} at height {height}",
        local_block.block_hash(),
    );
    ensure!(
        upstream_block.block_hash() == canonical_hash,
        "upstream certified block hash {} differs from canonical Reth hash {canonical_hash} at height {height}",
        upstream_block.block_hash(),
    );
    ensure!(
        local_finalization.proposal.payload.0 == canonical_hash,
        "local archived finalization payload {} differs from canonical Reth hash {canonical_hash} at height {height}",
        local_finalization.proposal.payload.0,
    );
    ensure!(
        upstream_finalization.proposal.payload.0 == canonical_hash,
        "upstream finalization payload {} differs from canonical Reth hash {canonical_hash} at height {height}",
        upstream_finalization.proposal.payload.0,
    );
    let local_epoch = local_finalization.proposal.round.epoch();
    let upstream_epoch = upstream_finalization.proposal.round.epoch();
    ensure!(
        local_epoch == upstream_epoch,
        "local archived finalization epoch {} differs from authenticated upstream epoch {} at height {height}",
        local_epoch.get(),
        upstream_epoch.get(),
    );
    let scheme = schemes.scoped(local_epoch).ok_or_else(|| {
        eyre::eyre!(
            "certified follower has no verifier scheme for recovery epoch {}",
            local_epoch.get()
        )
    })?;
    let mut local_rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    ensure!(
        local_finalization.verify(
            &mut local_rng,
            scheme.as_ref(),
            &commonware_parallel::Sequential,
        ),
        "local archived finalization certificate failed verification at height {height}",
    );
    let mut upstream_rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    ensure!(
        upstream_finalization.verify(
            &mut upstream_rng,
            scheme.as_ref(),
            &commonware_parallel::Sequential,
        ),
        "upstream finalization certificate failed verification at height {height}",
    );

    Ok(CertifiedFollowerRecoveryAnchor {
        checkpoint: ProjectionCheckpoint {
            block_number: height,
            block_hash: canonical_hash,
        },
        finalization: Some(local_finalization.clone()),
        block: local_block.clone(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::stack) struct RecoveredApplicationFinalization {
    pub(in crate::stack) round: Round,
    pub(in crate::stack) digest: Digest,
}

/// Promote the conservative cross-store anchor only when marshal proves the
/// exact canonical execution-head digest. Height agreement alone is not
/// sufficient: it could conceal a same-height fork after a partial restore.
pub(in crate::stack) fn reconcile_recovered_execution_head(
    last_execution_height: u64,
    last_execution_hash: B256,
    recovered: Option<RecoveredApplicationFinalization>,
) -> Result<(u64, B256, Option<Round>)> {
    if last_execution_height == 0 {
        ensure!(
            recovered.is_none(),
            "marshal returned a finalization record for genesis execution height"
        );
        return Ok((0, last_execution_hash, None));
    }

    let recovered = recovered.ok_or_else(|| {
        eyre::eyre!(
            "marshal returned no finalization for non-genesis execution height {last_execution_height}"
        )
    })?;
    ensure!(
        recovered.digest.0 == last_execution_hash,
        "marshal finalization digest mismatch at execution height {last_execution_height}: \
         execution={last_execution_hash}, marshal={}",
        recovered.digest.0
    );

    Ok((
        last_execution_height,
        last_execution_hash,
        Some(recovered.round),
    ))
}

/// Reconcile the derived CE tree only after startup has selected an exact
/// finality anchor backed by both the marshal archive and durable Reth state.
///
/// Marshal's processed height is an acknowledgement floor: it can lag the
/// archive by one block when the process stops after the CE commit but before
/// the ACK is durably recorded. It is therefore diagnostic context, not the
/// recovery authority.
pub(in crate::stack) fn recover_ce_at_reconciled_anchor(
    ce_startup_recovery: &dyn CeStartupRecovery,
    marshal_processed_height: u64,
    recovery_anchor_height: u64,
) -> Result<outbe_compressed_entities::FinalizedMarker> {
    let recovered = ce_startup_recovery
        .recover_before_participation(recovery_anchor_height)
        .wrap_err("compressed-tree startup recovery failed before validator participation")?;
    info!(
        marshal_processed_height,
        recovery_anchor_height,
        ce_marker_height = recovered.height,
        "marshal archive and compressed-tree recovery reconciled"
    );
    Ok(recovered)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::stack) fn validate_ancestor_follower_recovery_record(
    height: u64,
    canonical_hash: B256,
    local_finalization: Option<&outbe_consensus::marshal_types::Finalization>,
    local_block: &outbe_consensus::block::ConsensusBlock,
    upstream: &outbe_consensus::follow::upstream::AncestorFinalityProof,
    schemes: &HybridSchemeProvider<MinSig>,
    epocher: &outbe_consensus::follow::FollowerEpocher,
) -> Result<CertifiedFollowerRecoveryAnchor> {
    use commonware_consensus::types::Epocher as _;
    upstream.validate_envelope(Height::new(height))?;
    ensure!(
        local_block.number() == height && local_block.block_hash() == canonical_hash,
        "local recovery block differs from canonical Reth checkpoint"
    );
    ensure!(
        local_block == upstream.target(),
        "upstream recovery ancestor differs from local block"
    );
    let certified = &upstream.certified;
    let epoch = certified.finalization.proposal.round.epoch();
    ensure!(
        epocher
            .containing(Height::new(height))
            .is_some_and(|bounds| bounds.epoch() == epoch),
        "recovery ancestor certificate is not from its authenticated historical committee"
    );
    if upstream.ancestors.is_empty() {
        return validate_certified_follower_recovery_record(
            height,
            canonical_hash,
            local_finalization
                .ok_or_else(|| eyre::eyre!("missing normalized direct recovery certificate"))?,
            local_block,
            &certified.finalization,
            &certified.block,
            schemes,
        );
    }
    let scheme = schemes
        .scoped(epoch)
        .ok_or_else(|| eyre::eyre!("missing recovery ancestor committee"))?;
    ensure!(
        certified.finalization.verify(
            &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
            scheme.as_ref(),
            &commonware_parallel::Sequential
        ),
        "recovery descendant certificate failed verification"
    );
    if let Some(local) = local_finalization {
        ensure!(
            local.proposal.payload.0 == canonical_hash && local.proposal.round.epoch() == epoch,
            "local recovery certificate conflicts with authenticated ancestor"
        );
        ensure!(
            local.verify(
                &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                scheme.as_ref(),
                &commonware_parallel::Sequential
            ),
            "local recovery ancestor certificate failed verification"
        );
    }
    Ok(CertifiedFollowerRecoveryAnchor {
        checkpoint: ProjectionCheckpoint {
            block_number: height,
            block_hash: canonical_hash,
        },
        finalization: local_finalization.cloned(),
        block: local_block.clone(),
    })
}
