use super::super::*;

/// reth's canonical head can lead consensus finalization by the in-flight block
/// (steady state: `head_height = finalized_height + 1`, and a few during a
/// finalization hiccup). On a plain restart in that window, the head has no
/// finalization record yet. This is a normal unfinalized head, not archive
/// corruption. A head that leads the marshal finalized tip by at most this many
/// blocks is treated as that benign case. A larger lead is suspicious and stays
/// fatal.
pub const MAX_UNFINALIZED_HEAD_LEAD: u64 = 16;

/// Whether an execution head that leads the marshal's durable finalized tip is
/// the benign "unfinalized in-flight head" case rather than archive corruption.
/// The benign case is a real finalized tip (`> 0`) and a positive, bounded lead.
/// The caller still confirms that the marshal actually holds the finalized tip's
/// finalization record before it treats the restart as recoverable.
pub fn unfinalized_head_lead_is_recoverable(
    last_execution_height: u64,
    finalized_tip: u64,
) -> bool {
    let head_lead = last_execution_height.saturating_sub(finalized_tip);
    finalized_tip > 0 && head_lead > 0 && head_lead <= MAX_UNFINALIZED_HEAD_LEAD
}

/// Highest height that both the execution store and durable consensus finality
/// can authorize at startup. An execution-only head is speculative and must not
/// seed finalized forkchoice state. A consensus-only suffix is backfilled later.
pub fn durable_recovery_anchor_height(last_execution_height: u64, finalized_tip: u64) -> u64 {
    last_execution_height.min(finalized_tip)
}

/// Inclusive suffix that must be authenticated before a certified follower's
/// Marshal actor can repair or dispatch durable consensus records.
pub fn certified_follower_replay_suffix_bounds(
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
pub struct CertifiedFollowerRecoveryFloors {
    pub marshal_processed: u64,
    pub archive_finalization_tip: u64,
    pub archive_block_tip: u64,
    pub execution_tip: u64,
    pub reth_finalized: u64,
}

/// Select the only height that can still become the certified follower startup
/// anchor before exact identity and certificate verification. Heights merely
/// bound the candidate: they never authorize it without the later hash and
/// certificate checks.
pub fn select_certified_follower_recovery_height(
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
pub struct CertifiedFollowerRecoveryAnchor {
    pub checkpoint: ProjectionCheckpoint,
    pub finalization: Option<crate::marshal_types::Finalization>,
    pub block: crate::block::ConsensusBlock,
}

pub struct FollowerRecoveryBlock<'a> {
    pub checkpoint: ProjectionCheckpoint,
    pub block: &'a crate::block::ConsensusBlock,
}

fn check_ancestor_recovery_envelope_and_epoch(
    local: &FollowerRecoveryBlock<'_>,
    upstream: &crate::follow::upstream::AncestorFinalityProof,
    epocher: &crate::follow::FollowerEpocher,
) -> Result<Epoch> {
    use commonware_consensus::types::Epocher as _;
    let height = local.checkpoint.block_number;
    let canonical_hash = local.checkpoint.block_hash;
    upstream.validate_envelope(Height::new(height))?;
    if local.block.number() != height || local.block.block_hash() != canonical_hash {
        return Err(crate::errors::StackRecoveryError::LocalBlockDiffersFromCheckpoint.into());
    }
    if local.block != upstream.target() {
        return Err(
            crate::errors::StackRecoveryError::UpstreamAncestorDiffersFromLocalBlock.into(),
        );
    }
    let epoch = upstream.certified.finalization.proposal.round.epoch();
    if epocher
        .containing(Height::new(height))
        .is_none_or(|bounds| bounds.epoch() != epoch)
    {
        return Err(crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch.into());
    }
    Ok(epoch)
}

fn verify_ancestor_certificates(
    epoch: Epoch,
    canonical_hash: B256,
    certified_finalization: &crate::marshal_types::Finalization,
    local_finalization: Option<&crate::marshal_types::Finalization>,
    schemes: &HybridSchemeProvider<MinSig>,
) -> Result<()> {
    let scheme = schemes
        .scoped(epoch)
        .ok_or(crate::errors::StackRecoveryError::MissingRecoveryAncestorCommittee)?;
    let mut rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    if !certified_finalization.verify(&mut rng, scheme.as_ref(), &commonware_parallel::Sequential) {
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
    Ok(())
}

pub fn validate_ancestor_follower_recovery_record(
    local: FollowerRecoveryBlock<'_>,
    local_finalization: Option<&crate::marshal_types::Finalization>,
    upstream: &crate::follow::upstream::AncestorFinalityProof,
    schemes: &HybridSchemeProvider<MinSig>,
    epocher: &crate::follow::FollowerEpocher,
) -> Result<CertifiedFollowerRecoveryAnchor> {
    let epoch = check_ancestor_recovery_envelope_and_epoch(&local, upstream, epocher)?;
    let certified = &upstream.certified;
    if upstream.ancestors.is_empty() {
        return validate_certified_follower_recovery_record(
            local,
            local_finalization
                .ok_or(crate::errors::StackRecoveryError::MissingDirectRecoveryCertificate)?,
            &certified.finalization,
            &certified.block,
            schemes,
        );
    }
    verify_ancestor_certificates(
        epoch,
        local.checkpoint.block_hash,
        &certified.finalization,
        local_finalization,
        schemes,
    )?;
    Ok(CertifiedFollowerRecoveryAnchor {
        checkpoint: local.checkpoint,
        finalization: local_finalization.cloned(),
        block: local.block.clone(),
    })
}

fn check_certified_recovery_blocks(
    local: &FollowerRecoveryBlock<'_>,
    upstream_block: &crate::block::ConsensusBlock,
) -> Result<()> {
    let height = local.checkpoint.block_number;
    let canonical_hash = local.checkpoint.block_hash;
    if local.block.number() != height {
        return Err(
            crate::errors::StackRecoveryError::LocalArchivedBlockHeightMismatch {
                actual: local.block.number(),
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
    if local.block.block_hash() != canonical_hash {
        return Err(
            crate::errors::StackRecoveryError::LocalArchivedBlockHashMismatch {
                actual: local.block.block_hash(),
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
    Ok(())
}

fn check_certified_recovery_finalizations(
    checkpoint: ProjectionCheckpoint,
    local_finalization: &crate::marshal_types::Finalization,
    upstream_finalization: &crate::marshal_types::Finalization,
) -> Result<Epoch> {
    let height = checkpoint.block_number;
    let canonical_hash = checkpoint.block_hash;
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
    Ok(local_epoch)
}

pub fn validate_certified_follower_recovery_record(
    local: FollowerRecoveryBlock<'_>,
    local_finalization: &crate::marshal_types::Finalization,
    upstream_finalization: &crate::marshal_types::Finalization,
    upstream_block: &crate::block::ConsensusBlock,
    schemes: &HybridSchemeProvider<MinSig>,
) -> Result<CertifiedFollowerRecoveryAnchor> {
    check_certified_recovery_blocks(&local, upstream_block)?;
    let local_epoch = check_certified_recovery_finalizations(
        local.checkpoint,
        local_finalization,
        upstream_finalization,
    )?;
    let height = local.checkpoint.block_number;
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
        checkpoint: local.checkpoint,
        finalization: Some(local_finalization.clone()),
        block: local.block.clone(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveredApplicationFinalization {
    pub round: Round,
    pub digest: Digest,
}

/// Promote the conservative cross-store anchor only when marshal proves the
/// exact canonical execution-head digest. Height agreement alone is not
/// sufficient: it could conceal a same-height fork after a partial restore.
pub fn reconcile_recovered_execution_head(
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

/// Canonical header summary read from [`CanonicalHistorySource`] for `c ..= H`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalHeaderRecord {
    pub number: u64,
    pub hash: B256,
    pub parent_hash: B256,
    pub extra_data: Bytes,
}

/// Read-only canonical chain-state interface at or below the recovery checkpoint.
pub trait CanonicalHistorySource {
    fn genesis_participants(&self) -> Result<commonware_utils::ordered::Set<bls12381::PublicKey>>;
    fn canonical_header(&self, height: u64) -> Result<Option<CanonicalHeaderRecord>>;
    fn committed_committee_snapshot(
        &self,
        checkpoint_block_hash: B256,
        epoch: u64,
    ) -> Result<Option<crate::proof::CommitteeSnapshot>>;
}

pub struct NodeCanonicalHistory<'a, N: StackNodeAccess> {
    pub protocol: &'a dyn crate::protocol_state::EngineProtocolState,
    pub node: &'a N,
    pub genesis_hash: B256,
}

impl<N: StackNodeAccess> CanonicalHistorySource for NodeCanonicalHistory<'_, N> {
    fn genesis_participants(&self) -> Result<commonware_utils::ordered::Set<bls12381::PublicKey>> {
        let validator_set = validators::read_consensus_validators_at_block(
            self.protocol,
            self.node.provider(),
            self.genesis_hash,
        )?;
        participants_from_validator_set(&validator_set)
    }

    fn canonical_header(&self, height: u64) -> Result<Option<CanonicalHeaderRecord>> {
        let Some(sealed) = self
            .node
            .provider()
            .sealed_header(height)
            .map_err(|error| crate::errors::StackDkgError::ReadHeaderAtHeight {
                height,
                reason: format!("{error}"),
            })?
        else {
            return Ok(None);
        };
        Ok(Some(CanonicalHeaderRecord {
            number: sealed.header().inner.number,
            hash: sealed.hash(),
            parent_hash: sealed.header().inner.parent_hash,
            extra_data: sealed.header().inner.extra_data.clone(),
        }))
    }

    fn committed_committee_snapshot(
        &self,
        checkpoint_block_hash: B256,
        epoch: u64,
    ) -> Result<Option<crate::proof::CommitteeSnapshot>> {
        let state = self
            .node
            .provider()
            .state_by_block_hash(checkpoint_block_hash)
            .map_err(|error| crate::errors::ValidatorsError::GetStateAtBlock {
                block_hash: checkpoint_block_hash,
                error: format!("{error}"),
            })?;
        validators::read_committee_snapshot_from_state(self.protocol, &state, epoch)
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Clone, Debug)]
pub struct StaticCanonicalHistory {
    pub genesis_participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
    pub headers: std::collections::BTreeMap<u64, CanonicalHeaderRecord>,
    pub snapshots: std::collections::BTreeMap<(B256, u64), crate::proof::CommitteeSnapshot>,
}

#[cfg(any(test, feature = "test-utils"))]
impl CanonicalHistorySource for StaticCanonicalHistory {
    fn genesis_participants(&self) -> Result<commonware_utils::ordered::Set<bls12381::PublicKey>> {
        Ok(self.genesis_participants.clone())
    }

    fn canonical_header(&self, height: u64) -> Result<Option<CanonicalHeaderRecord>> {
        Ok(self.headers.get(&height).cloned())
    }

    fn committed_committee_snapshot(
        &self,
        checkpoint_block_hash: B256,
        epoch: u64,
    ) -> Result<Option<crate::proof::CommitteeSnapshot>> {
        Ok(self.snapshots.get(&(checkpoint_block_hash, epoch)).cloned())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RecoveryCommitteeConfig {
    pub epoch_length_blocks: u64,
    pub prepare_window_blocks: u64,
    pub activation_grace_blocks: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedApplicationFinalization {
    pub round: Round,
    pub digest: Digest,
    pub parent_proof_record: crate::finalization::parent_cert_store::CertifiedParentProofRecord,
}

/// Promote the conservative cross-store anchor only when authenticated Marshal
/// history (direct `F(H)` or descendant-certified sparse `H < K`) proves the
/// exact canonical execution-head digest and its actual-kind parent proof record.
pub fn reconcile_authenticated_execution_head(
    target_height: u64,
    expected_hash: B256,
    recovered: Option<AuthenticatedApplicationFinalization>,
) -> Result<(
    u64,
    B256,
    Option<Round>,
    Option<crate::finalization::parent_cert_store::CertifiedParentProofRecord>,
)> {
    if target_height == 0 {
        if recovered.is_some() {
            return Err(crate::errors::StackRecoveryError::MarshalFinalizationAtGenesis.into());
        }
        return Ok((0, expected_hash, None, None));
    }

    let recovered = recovered.ok_or(
        crate::errors::StackRecoveryError::MarshalFinalizationMissingForHeight(target_height),
    )?;
    if recovered.digest.0 != expected_hash {
        return Err(
            crate::errors::StackRecoveryError::MarshalFinalizationDigestMismatch {
                height: target_height,
                execution: expected_hash,
                marshal: recovered.digest.0,
            }
            .into(),
        );
    }

    Ok((
        target_height,
        expected_hash,
        Some(recovered.round),
        Some(recovered.parent_proof_record),
    ))
}

/// Ephemeral private historical-committee verifier authority used solely during
/// startup finality recovery. Owns private `HybridSchemeProvider` and
/// `CommitteeProvider` instances that are dropped when recovery returns, so live
/// startup providers are never mutated by recovery.
pub(crate) struct RecoveryCommitteeAuthority {
    schemes: HybridSchemeProvider<MinSig>,
    committees: CommitteeProvider,
    chain: crate::follow::CommitteeChain,
    epocher: crate::follow::FollowerEpocher,
    current_epoch: Epoch,
    pending_preannounce: Option<(u64, Vec<u8>)>,
}

impl RecoveryCommitteeAuthority {
    pub(crate) fn init_at_checkpoint<S: CanonicalHistorySource>(
        source: &S,
        config: RecoveryCommitteeConfig,
        target_height: u64,
        expected_hash: B256,
    ) -> Result<Self> {
        use commonware_consensus::types::Epocher as _;

        let max_scan = config
            .epoch_length_blocks
            .saturating_add(config.prepare_window_blocks)
            .saturating_add(config.activation_grace_blocks)
            .saturating_mul(2)
            .max(10_000);
        let min_height = target_height.saturating_sub(max_scan).max(1);

        let mut expected_child_parent = expected_hash;
        let mut headers_above_carrier: Vec<CanonicalHeaderRecord> = Vec::new();
        let mut anchor_carrier: Option<(u64, DkgBoundaryArtifact)> = None;

        for height in (min_height..=target_height).rev() {
            let Some(hdr) = source.canonical_header(height)? else {
                return Err(
                    crate::errors::StackRecoveryError::MissingRecoveryAncestorCommittee.into(),
                );
            };
            if hdr.number != height || hdr.hash != expected_child_parent {
                return Err(
                    crate::errors::StackRecoveryError::LocalBlockDiffersFromCheckpoint.into(),
                );
            }
            let artifacts =
                decode_outbe_block_artifacts(hdr.extra_data.as_ref()).map_err(|error| {
                    crate::errors::StackDkgError::DecodeHeaderArtifactsAtHeight {
                        height,
                        reason: format!("{error}"),
                    }
                })?;
            if let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) =
                artifacts.consensus_header_artifact
            {
                anchor_carrier = Some((height, boundary));
                break;
            }
            expected_child_parent = hdr.parent_hash;
            headers_above_carrier.push(hdr);
        }

        let Some((carrier_height, boundary)) = anchor_carrier else {
            return Err(crate::errors::StackRecoveryError::MissingRecoveryAncestorCommittee.into());
        };
        if carrier_height == 0 {
            return Err(crate::errors::StackRecoveryError::MissingRecoveryAncestorCommittee.into());
        }

        let anchor_epoch = Epoch::new(boundary.epoch);
        let snapshot = source
            .committed_committee_snapshot(expected_hash, anchor_epoch.get())?
            .ok_or(crate::errors::StackRecoveryError::MissingRecoveryAncestorCommittee)?;
        if snapshot.vrf_material_version != boundary.vrf_material_version {
            return Err(
                crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch.into(),
            );
        }

        let boundary_output = dkg::persistence::decode_boundary_output(&boundary)?;
        let (chain, players) = if anchor_epoch == Epoch::new(0) {
            let mut chain =
                crate::follow::CommitteeChain::new(Epoch::new(0), source.genesis_participants()?);
            let players = chain.register_epoch_from_outcome(Epoch::new(0), &boundary.outcome)?;
            (chain, players)
        } else {
            let chain = crate::follow::CommitteeChain::from_trusted_local_boundary(
                anchor_epoch,
                &boundary.outcome,
            )?;
            (chain, boundary_output.players().clone())
        };

        let ordered_addresses = ordered_addresses_from_recovered_boundary(&players, &boundary)?;
        let verifier_scheme = HybridScheme::<MinSig>::verifier_with_vrf_provider(
            &config::outbe_app_namespace(),
            players,
            VrfMaterialProvider::new(
                boundary.vrf_material_version,
                boundary_output.public().clone(),
                None,
            ),
        )
        .ok_or(crate::errors::StackRecoveryError::MissingRecoveryAncestorCommittee)?;
        let prelude = crate::finalization::committee_prelude::build_committee_prelude(
            &verifier_scheme,
            &ordered_addresses,
            anchor_epoch.get(),
        )?;
        if prelude.committee_set_hash != snapshot.committee_set_hash_v2(anchor_epoch.get()) {
            return Err(
                crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch.into(),
            );
        }

        let schemes = HybridSchemeProvider::<MinSig>::new();
        let committees = CommitteeProvider::new();
        schemes.register(anchor_epoch, verifier_scheme);
        committees.register(anchor_epoch, ordered_addresses);

        let epocher = crate::follow::FollowerEpocher::from_anchor(
            config.epoch_length_blocks,
            config.activation_grace_blocks,
            anchor_epoch,
            Height::new(carrier_height),
        );
        if epocher
            .containing(Height::new(target_height))
            .is_none_or(|bounds| bounds.epoch() != anchor_epoch)
        {
            return Err(
                crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch.into(),
            );
        }

        let mut pending_preannounce = None;
        for hdr in headers_above_carrier.into_iter().rev() {
            let artifacts =
                decode_outbe_block_artifacts(hdr.extra_data.as_ref()).map_err(|error| {
                    crate::errors::StackDkgError::DecodeHeaderArtifactsAtHeight {
                        height: hdr.number,
                        reason: format!("{error}"),
                    }
                })?;
            if let Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, .. }) =
                artifacts.consensus_header_artifact
            {
                if anchor_epoch.get().checked_add(1) != Some(epoch) {
                    return Err(crate::errors::FollowError::PreAnnouncesNonSuccessor {
                        certified: anchor_epoch.get(),
                        announced: epoch,
                    }
                    .into());
                }
                pending_preannounce = Some((epoch, hdr.extra_data.to_vec()));
            }
        }

        Ok(Self {
            schemes,
            committees,
            chain,
            epocher,
            current_epoch: anchor_epoch,
            pending_preannounce,
        })
    }

    pub(crate) fn advance_block(
        &mut self,
        prev_block: &crate::block::ConsensusBlock,
        block_k: &crate::block::ConsensusBlock,
    ) -> Result<()> {
        use commonware_consensus::types::Epocher as _;
        use outbe_primitives::consensus_metadata::ParentParticipationProof;

        let extra_data = block_k.header().inner.extra_data.as_ref();
        let artifacts = decode_outbe_block_artifacts(extra_data).map_err(|error| {
            crate::errors::StackDkgError::DecodeHeaderArtifactsAtHeight {
                height: block_k.number(),
                reason: format!("{error}"),
            }
        })?;

        match artifacts.consensus_header_artifact {
            Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, .. }) => {
                if self.current_epoch.get().checked_add(1) != Some(epoch) {
                    return Err(crate::errors::FollowError::PreAnnouncesNonSuccessor {
                        certified: self.current_epoch.get(),
                        announced: epoch,
                    }
                    .into());
                }
                self.pending_preannounce = Some((epoch, extra_data.to_vec()));
            }
            Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) => {
                let next_epoch = Epoch::new(boundary.epoch);
                if self.current_epoch.get().checked_add(1) != Some(next_epoch.get()) {
                    return Err(
                        crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch
                            .into(),
                    );
                }

                // D1 anti-self-certification gate: require an Epoch(E)-quorum
                // parent witness on `block_k` over `prev_block` before admitting
                // `BoundaryOutcome(E + 1)`.
                let bw = crate::finalization::evidence::verify_child_parent_witness(
                    prev_block,
                    block_k,
                    &self.schemes,
                    &self.committees,
                )?
                .ok_or_else(|| {
                    crate::errors::StackRecoveryError::MissingRecoveryEpochVerifierScheme(
                        self.current_epoch.get(),
                    )
                })?;
                if bw.round.epoch() != self.current_epoch {
                    return Err(
                        crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch
                            .into(),
                    );
                }
                if bw.proof_kind == ParentParticipationProof::Finalization {
                    let f_ref = bw.finalization.as_ref().ok_or(
                        crate::errors::StackRecoveryError::MissingDirectRecoveryCertificate,
                    )?;
                    let _ = crate::finalization::evidence::verify_direct_finalization(
                        prev_block.number(),
                        f_ref,
                        &self.schemes,
                        &self.committees,
                    )?
                    .ok_or_else(|| {
                        crate::errors::StackRecoveryError::MissingRecoveryEpochVerifierScheme(
                            self.current_epoch.get(),
                        )
                    })?;
                }

                let Some((_, pre_extra)) = self.pending_preannounce.take() else {
                    return Err(crate::errors::FollowError::MissingCommitteePreAnnounce {
                        epoch: next_epoch.get(),
                        boundary_height: block_k.number(),
                    }
                    .into());
                };
                self.chain.admit_authenticated_header(
                    self.current_epoch,
                    &pre_extra,
                    crate::follow::AdmissionPolicy::Routed {
                        routed_epoch: self.current_epoch,
                    },
                )?;
                self.chain.admit_authenticated_header(
                    next_epoch,
                    extra_data,
                    crate::follow::AdmissionPolicy::Routed {
                        routed_epoch: self.current_epoch,
                    },
                )?;
                self.epocher
                    .observe_boundary(next_epoch, Height::new(block_k.number()))
                    .map_err(|source| {
                        crate::errors::StackEpochError::EpochBoundaryObservationFailed {
                            epoch: next_epoch,
                            height: block_k.number(),
                            source,
                        }
                    })?;

                let boundary_output = dkg::persistence::decode_boundary_output(&boundary)?;
                let players_next = boundary_output.players().clone();
                let ordered_addresses =
                    ordered_addresses_from_recovered_boundary(&players_next, &boundary)?;
                let scheme_next = HybridScheme::<MinSig>::verifier_with_vrf_provider(
                    &config::outbe_app_namespace(),
                    players_next,
                    VrfMaterialProvider::new(
                        boundary.vrf_material_version,
                        boundary_output.public().clone(),
                        None,
                    ),
                )
                .ok_or(crate::errors::StackRecoveryError::MissingRecoveryAncestorCommittee)?;
                let prelude = crate::finalization::committee_prelude::build_committee_prelude(
                    &scheme_next,
                    &ordered_addresses,
                    next_epoch.get(),
                )?;
                if prelude.committee_set_hash != boundary.committee_set_hash {
                    return Err(
                        crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch
                            .into(),
                    );
                }
                self.schemes.register(next_epoch, scheme_next);
                self.committees.register(next_epoch, ordered_addresses);
                self.current_epoch = next_epoch;
            }
            Some(ConsensusHeaderArtifact::DealerLog(_)) | None => {}
        }

        if self
            .epocher
            .containing(Height::new(block_k.number()))
            .is_none_or(|bounds| bounds.epoch() != self.current_epoch)
        {
            return Err(
                crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch.into(),
            );
        }
        Ok(())
    }

    pub(crate) fn current_epoch(&self) -> Epoch {
        self.current_epoch
    }

    pub(crate) fn schemes(&self) -> &HybridSchemeProvider<MinSig> {
        &self.schemes
    }

    pub(crate) fn committees(&self) -> &CommitteeProvider {
        &self.committees
    }

    pub(crate) fn epocher(&self) -> &crate::follow::FollowerEpocher {
        &self.epocher
    }

    pub(crate) fn build_finalization_record(
        &self,
        height: u64,
        finalization: &crate::marshal_types::Finalization,
    ) -> Result<crate::finalization::parent_cert_store::CertifiedParentProofRecord> {
        let epoch = finalization.proposal.round.epoch();
        let (Some(scheme), Some(committee)) = (
            self.schemes.scoped(epoch),
            self.committees.ordered_committee(epoch),
        ) else {
            return Err(
                crate::errors::StackRecoveryError::MissingRecoveryEpochVerifierScheme(epoch.get())
                    .into(),
            );
        };
        let encoded_certificate: Bytes = commonware_codec::Encode::encode(finalization).into();
        crate::finalization::resolver::build_recovered_finalization_record(
            crate::finalization::resolver::RecoveredFinalizedBlock::from_proposal(
                height,
                &finalization.proposal,
            ),
            crate::finalization::resolver::RecoveredFinalizationMaterial {
                ordered_committee: committee.as_ref(),
                certificate: &finalization.certificate,
                encoded_certificate,
                scheme: scheme.as_ref(),
            },
        )
        .map_err(Into::into)
    }

    pub(crate) fn build_cn_record(
        witness: crate::finalization::evidence::VerifiedChildParentWitness,
        digest: Digest,
    ) -> crate::finalization::parent_cert_store::CertifiedParentProofRecord {
        crate::finalization::actor::build_cn_record(witness, digest)
    }
}
