//! Promotion of the committed DKG boundary's threshold material to durable
//! storage.
//!
//! The execution-height arm used to be the only place that took the committed
//! boundary, and that arm is disabled while a reshare runs. When the next
//! ceremony completed first, recording its boundary dropped the untaken commit
//! and the active share never reached disk. The epoch loop therefore promotes
//! from every point that can precede that loss: the height arm, the tip arm
//! during a reshare, and the start of each ceremony-completion arm.

use super::super::*;

/// Which pending-DKG state a promotion retires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::stack) enum RetireScope {
    /// No later ceremony is running: retire pending material and the retry store.
    All,
    /// A later ceremony is running or just completed: its retry store stays.
    PendingMaterialOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::stack) enum BoundaryPromotion {
    /// No committed boundary was waiting to be taken.
    NothingCommitted,
    /// The committed boundary's material is durable and pending state retired.
    Promoted,
    /// The finalized boundary excludes the local key: leave validator mode.
    LocalExcluded,
}

/// The node's currently active threshold material, which the committed
/// boundary must match to be promoted.
pub(in crate::stack) struct ActiveDkgMaterial<'a> {
    pub local_key: &'a bls12381::PublicKey,
    pub output: Option<&'a Output<MinSig, bls12381::PublicKey>>,
    pub share: Option<&'a Share>,
    pub polynomial: &'a Sharing<MinSig>,
}

/// Take the committed boundary, if any, and make the active material that
/// matches it durable.
pub(in crate::stack) async fn promote_committed_boundary(
    dkg_manager: &DkgManagerMailbox,
    keys_dir: Option<&std::path::Path>,
    key_backend: &bls::KeyBackend,
    active: ActiveDkgMaterial<'_>,
    scope: RetireScope,
) -> Result<BoundaryPromotion> {
    let Some(boundary) = dkg_manager.take_committed_boundary_artifact().await else {
        return Ok(BoundaryPromotion::NothingCommitted);
    };
    let boundary_output = decode_boundary_output(&boundary)
        .wrap_err("failed to decode finalized DKG boundary output")?;
    if active.output != Some(&boundary_output) {
        if boundary_output
            .players()
            .position(active.local_key)
            .is_none()
        {
            if let Some(keys_dir) = keys_dir {
                retire_activated_dkg_retry_state(keys_dir, key_backend)?;
            }
            info!(
                dkg_output_hash = %dkg_manager::dkg_output_hash(&boundary_output),
                "finalized DKG boundary excludes local validator; exiting validator mode"
            );
            return Ok(BoundaryPromotion::LocalExcluded);
        }
        return Err(eyre::eyre!(
            "finalized DKG boundary output does not match active local DKG output"
        ));
    }
    let Some(keys_dir) = keys_dir else {
        return Ok(BoundaryPromotion::Promoted);
    };
    if let Some(share) = active.share {
        save_dkg_state(
            keys_dir,
            share,
            active.polynomial,
            &boundary_output,
            key_backend,
        )
        .wrap_err("failed to promote finalized DKG state to disk")?;
        info!(
            keys_dir = %keys_dir.display(),
            dkg_output_hash = %dkg_manager::dkg_output_hash(&boundary_output),
            "promoted finalized DKG state to durable storage"
        );
    } else {
        info!(
            keys_dir = %keys_dir.display(),
            dkg_output_hash = %dkg_manager::dkg_output_hash(&boundary_output),
            "finalized DKG boundary adopted in verifier mode; no private share to promote"
        );
    }
    match scope {
        RetireScope::All => retire_activated_dkg_retry_state(keys_dir, key_backend)?,
        RetireScope::PendingMaterialOnly => {
            remove_pending_dkg_state(keys_dir);
            clear_pending_dkg_boundary(keys_dir);
        }
    }
    Ok(BoundaryPromotion::Promoted)
}

/// Commit the pending boundary from its finalized carrier on the canonical
/// chain when live finalization delivery never did. A boundary always rides the
/// first block after its activation anchor, so only `activation_anchor + 1` is
/// read, and only once it is finalized. Returns whether the carrier was adopted.
pub(in crate::stack) fn adopt_finalized_boundary_carrier(
    dkg_manager: &DkgManagerMailbox,
    provider: &(impl HeaderProvider<Header = OutbeHeader> + BlockHashReader),
    activation_anchor: u64,
    finalized_height: u64,
) -> Result<bool> {
    let Some(pending) = dkg_manager.uncommitted_pending_boundary() else {
        return Ok(false);
    };
    let carrier_height = activation_anchor.saturating_add(1);
    if carrier_height > finalized_height {
        return Ok(false);
    }
    let Some(header) = provider.sealed_header(carrier_height).map_err(|error| {
        eyre::eyre!("failed to read finalized boundary carrier {carrier_height}: {error}")
    })?
    else {
        return Ok(false);
    };
    let artifacts = decode_outbe_block_artifacts(header.header().inner.extra_data.as_ref())
        .map_err(|error| {
            eyre::eyre!(
                "failed to decode finalized boundary carrier artifacts at {carrier_height}: {error}"
            )
        })?;
    match artifacts.consensus_header_artifact {
        Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) if boundary == pending => {
            dkg_manager.adopt_finalized_boundary(carrier_height, header.hash(), &boundary)
        }
        Some(
            ConsensusHeaderArtifact::BoundaryOutcome(_)
            | ConsensusHeaderArtifact::DealerLog(_)
            | ConsensusHeaderArtifact::CommitteePreAnnounce { .. },
        )
        | None => Ok(false),
    }
}
