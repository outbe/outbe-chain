use super::super::*;

/// reth's canonical head can lead consensus finalization by the in-flight block
/// (steady state: `head_height = finalized_height + 1`, and a few during a
/// finalization hiccup). On a plain restart in that window, the head has no
/// finalization record yet. This is a normal unfinalized head, not archive
/// corruption. A head that leads the marshal finalized tip by at most this many
/// blocks is treated as that benign case. A larger lead is suspicious and stays
/// fatal.
pub(in crate::stack) const MAX_UNFINALIZED_HEAD_LEAD: u64 = 16;

/// Whether an execution head that leads the marshal's durable finalized tip is
/// the benign "unfinalized in-flight head" case rather than archive corruption.
/// The benign case is a real finalized tip (`> 0`) and a positive, bounded lead.
/// The caller still confirms that the marshal actually holds the finalized tip's
/// finalization record before it treats the restart as recoverable.
pub(in crate::stack) fn unfinalized_head_lead_is_recoverable(
    last_execution_height: u64,
    finalized_tip: u64,
) -> bool {
    let head_lead = last_execution_height.saturating_sub(finalized_tip);
    finalized_tip > 0 && head_lead > 0 && head_lead <= MAX_UNFINALIZED_HEAD_LEAD
}

/// Highest height that both the execution store and durable consensus finality
/// can authorize at startup. An execution-only head is speculative and must not
/// seed finalized forkchoice state. A consensus-only suffix is backfilled later.
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
    if floors.marshal_processed > anchor {
        return Err(
            crate::errors::StackRecoveryError::MarshalFloorExceedsAnchor {
                marshal_processed: floors.marshal_processed,
                anchor,
            }
            .into(),
        );
    }
    if anchor > floors.marshal_processed.saturating_add(1) {
        return Err(
            crate::errors::StackRecoveryError::AnchorExceedsMarshalFloorByMoreThanOne {
                anchor,
                marshal_processed: floors.marshal_processed,
            }
            .into(),
        );
    }
    if floors.reth_finalized > anchor {
        return Err(
            crate::errors::StackRecoveryError::RethFinalizedExceedsAnchor {
                reth_finalized: floors.reth_finalized,
                anchor,
            }
            .into(),
        );
    }
    Ok(anchor)
}

#[derive(Clone, Debug)]
pub(in crate::stack) struct CertifiedFollowerRecoveryAnchor {
    pub(in crate::stack) checkpoint: ProjectionCheckpoint,
    pub(in crate::stack) finalization: Option<outbe_consensus::marshal_types::Finalization>,
    pub(in crate::stack) block: outbe_consensus::block::ConsensusBlock,
}

pub(in crate::stack) struct FollowerRecoveryBlock<'a> {
    pub(in crate::stack) checkpoint: ProjectionCheckpoint,
    pub(in crate::stack) block: &'a outbe_consensus::block::ConsensusBlock,
}

pub(in crate::stack) fn validate_ancestor_follower_recovery_record(
    local: FollowerRecoveryBlock<'_>,
    local_finalization: Option<&outbe_consensus::marshal_types::Finalization>,
    upstream: &outbe_consensus::follow::upstream::AncestorFinalityProof,
    schemes: &HybridSchemeProvider<MinSig>,
    epocher: &outbe_consensus::follow::FollowerEpocher,
) -> Result<CertifiedFollowerRecoveryAnchor> {
    let FollowerRecoveryBlock {
        checkpoint:
            ProjectionCheckpoint {
                block_number: height,
                block_hash: canonical_hash,
            },
        block: local_block,
    } = local;
    use commonware_consensus::types::Epocher as _;
    upstream.validate_envelope(Height::new(height))?;
    if local_block.number() != height || local_block.block_hash() != canonical_hash {
        return Err(crate::errors::StackRecoveryError::LocalBlockDiffersFromCheckpoint.into());
    }
    if local_block != upstream.target() {
        return Err(
            crate::errors::StackRecoveryError::UpstreamAncestorDiffersFromLocalBlock.into(),
        );
    }
    let certified = &upstream.certified;
    let epoch = certified.finalization.proposal.round.epoch();
    if !epocher
        .containing(Height::new(height))
        .is_some_and(|bounds| bounds.epoch() == epoch)
    {
        return Err(crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch.into());
    }
    if upstream.ancestors.is_empty() {
        return validate_certified_follower_recovery_record(
            crate::stack::recovery::anchor::FollowerRecoveryBlock {
                checkpoint: ProjectionCheckpoint {
                    block_number: height,
                    block_hash: canonical_hash,
                },
                block: local_block,
            },
            local_finalization
                .ok_or(crate::errors::StackRecoveryError::MissingDirectRecoveryCertificate)?,
            &certified.finalization,
            &certified.block,
            schemes,
        );
    }
    let scheme = schemes
        .scoped(epoch)
        .ok_or(crate::errors::StackRecoveryError::MissingRecoveryAncestorCommittee)?;
    let mut rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    if !certified
        .finalization
        .verify(&mut rng, scheme.as_ref(), &commonware_parallel::Sequential)
    {
        return Err(
            crate::errors::StackRecoveryError::DescendantCertificateVerificationFailed.into(),
        );
    }
    if let Some(local) = local_finalization {
        if local.proposal.payload.0 != canonical_hash || local.proposal.round.epoch() != epoch {
            return Err(
                crate::errors::StackRecoveryError::LocalCertificateConflictsWithAncestor.into(),
            );
        }
        let mut rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
        if !local.verify(&mut rng, scheme.as_ref(), &commonware_parallel::Sequential) {
            return Err(
                crate::errors::StackRecoveryError::LocalAncestorCertificateVerificationFailed
                    .into(),
            );
        }
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

pub(in crate::stack) fn validate_certified_follower_recovery_record(
    local: FollowerRecoveryBlock<'_>,
    local_finalization: &outbe_consensus::marshal_types::Finalization,
    upstream_finalization: &outbe_consensus::marshal_types::Finalization,
    upstream_block: &outbe_consensus::block::ConsensusBlock,
    schemes: &HybridSchemeProvider<MinSig>,
) -> Result<CertifiedFollowerRecoveryAnchor> {
    let FollowerRecoveryBlock {
        checkpoint:
            ProjectionCheckpoint {
                block_number: height,
                block_hash: canonical_hash,
            },
        block: local_block,
    } = local;

    if local_block.number() != height {
        return Err(
            crate::errors::StackRecoveryError::LocalArchivedBlockHeightMismatch {
                actual: local_block.number(),
                expected: height,
            }
            .into(),
        );
    }
    if upstream_block.number() != height {
        return Err(
            crate::errors::StackRecoveryError::UpstreamCertifiedBlockHeightMismatch {
                actual: upstream_block.number(),
                expected: height,
            }
            .into(),
        );
    }
    if local_block.block_hash() != canonical_hash {
        return Err(
            crate::errors::StackRecoveryError::LocalArchivedBlockHashMismatch {
                actual: local_block.block_hash(),
                canonical_hash,
                height,
            }
            .into(),
        );
    }
    if upstream_block.block_hash() != canonical_hash {
        return Err(
            crate::errors::StackRecoveryError::UpstreamCertifiedBlockHashMismatch {
                actual: upstream_block.block_hash(),
                canonical_hash,
                height,
            }
            .into(),
        );
    }
    if local_finalization.proposal.payload.0 != canonical_hash {
        return Err(
            crate::errors::StackRecoveryError::LocalFinalizationPayloadMismatch {
                actual: local_finalization.proposal.payload.0,
                canonical_hash,
                height,
            }
            .into(),
        );
    }
    if upstream_finalization.proposal.payload.0 != canonical_hash {
        return Err(
            crate::errors::StackRecoveryError::UpstreamFinalizationPayloadMismatch {
                actual: upstream_finalization.proposal.payload.0,
                canonical_hash,
                height,
            }
            .into(),
        );
    }
    let local_epoch = local_finalization.proposal.round.epoch();
    let upstream_epoch = upstream_finalization.proposal.round.epoch();
    if local_epoch != upstream_epoch {
        return Err(
            crate::errors::StackRecoveryError::FinalizationEpochMismatch {
                local_epoch: local_epoch.get(),
                upstream_epoch: upstream_epoch.get(),
                height,
            }
            .into(),
        );
    }
    let scheme = schemes.scoped(local_epoch).ok_or_else(|| {
        crate::errors::StackRecoveryError::MissingRecoveryEpochVerifierScheme(local_epoch.get())
    })?;
    let mut local_rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    if !local_finalization.verify(
        &mut local_rng,
        scheme.as_ref(),
        &commonware_parallel::Sequential,
    ) {
        return Err(
            crate::errors::StackRecoveryError::LocalFinalizationVerificationFailed(height).into(),
        );
    }
    let mut upstream_rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    if !upstream_finalization.verify(
        &mut upstream_rng,
        scheme.as_ref(),
        &commonware_parallel::Sequential,
    ) {
        return Err(
            crate::errors::StackRecoveryError::UpstreamFinalizationVerificationFailed(height)
                .into(),
        );
    }

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
        if recovered.is_some() {
            return Err(crate::errors::StackRecoveryError::MarshalFinalizationAtGenesis.into());
        }
        return Ok((0, last_execution_hash, None));
    }

    let recovered = recovered.ok_or(
        crate::errors::StackRecoveryError::MarshalFinalizationMissingForHeight(
            last_execution_height,
        ),
    )?;
    if recovered.digest.0 != last_execution_hash {
        return Err(
            crate::errors::StackRecoveryError::MarshalFinalizationDigestMismatch {
                height: last_execution_height,
                execution: last_execution_hash,
                marshal: recovered.digest.0,
            }
            .into(),
        );
    }

    Ok((
        last_execution_height,
        last_execution_hash,
        Some(recovered.round),
    ))
}

/// Reconcile the derived CE tree only after startup has selected an exact
/// finality anchor backed by both the marshal archive and durable Reth state.
///
/// Marshal's processed height is an acknowledgement floor. It can lag the
/// archive by one block. This happens when the process stops after the CE commit
/// but before the ACK is durably recorded. It is therefore diagnostic context,
/// not the recovery authority.
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
