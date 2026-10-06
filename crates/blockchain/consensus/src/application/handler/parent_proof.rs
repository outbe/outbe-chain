use super::ApplicationShared;

use crate::block::ConsensusBlock;

use crate::digest::Digest;

use crate::finalization::parent_cert_store::CertifiedParentProofKey;
use crate::finalization::parent_cert_store::CertifiedParentProofRecord;

use alloy_consensus::Transaction as _;

use commonware_consensus::types::Height;
use commonware_consensus::types::Round;
use commonware_consensus::types::View;

use outbe_primitives::reshare_artifact::FinalizedParentAttestation;

use tracing::info;
use tracing::warn;

pub(super) fn finalized_parent_attestation_from_phase1_system_tx(
    block: &ConsensusBlock,
) -> eyre::Result<Option<FinalizedParentAttestation>> {
    if block.number() < 2 {
        return Ok(None);
    }

    let raw_block = block.clone().into_inner().into_block();
    let layout = outbe_primitives::system_tx::split_system_layout(&raw_block.body.transactions)
        .map_err(|error| {
            eyre::eyre!("invalid system tx layout while extracting Phase 1: {error}")
        })?;
    let phase1 = *layout.begin.first().ok_or_else(|| {
        eyre::eyre!(
            "missing Phase 1 finalization system transaction for block {}",
            block.number()
        )
    })?;
    let input = outbe_primitives::system_tx::SystemTxInputV2::decode(phase1.input().as_ref())
        .map_err(|error| eyre::eyre!("decode Phase 1 system transaction input: {error}"))?;
    let outbe_primitives::system_tx::SystemTxInputV2::CertifiedParentAccounting { metadata } =
        input
    else {
        return Err(eyre::eyre!(
            "expected Phase 1 finalization system transaction at begin ordinal 0"
        ));
    };

    Ok(Some(FinalizedParentAttestation {
        finalized_block_number: metadata.finalized_block_number,
        finalized_block_hash: metadata.finalized_block_hash,
        finalized_epoch: metadata.finalized_epoch,
        finalized_view: metadata.finalized_view,
        parent_view: metadata.parent_view,
        ordered_committee: metadata.ordered_committee,
        signer_bitmap: metadata.signer_bitmap,
        certificate: metadata.proof,
        // V2 `missed_proposers: Vec<MissedProposerEvent>` is always
        // empty under the verifier rule. The V1 attestation surface
        // keeps the legacy `Vec<Address>` shape for backwards-compat callers.
        missed_proposers: metadata
            .missed_proposers
            .into_iter()
            .map(|ev| ev.validator)
            .collect(),
    }))
}

// Like `BuildBlockOutcome` above, this is an internal direct-parent proof lookup
// result. `select_parent_proof_for_proposal` produces it once per proposal, and the
// single match site consumes it immediately. `Found` is the common case, so boxing the
// record would only add a heap allocation on the hot proposer path for no benefit.
#[allow(clippy::large_enum_variant)]
pub(super) enum ParentProofLookup {
    NoProofNeeded,
    Found(CertifiedParentProofRecord),
    Unavailable,
}

// Marshal-based block resolution tests are in `crate::marshal_tests`.

pub(crate) fn parent_round(round: Round, parent_view: View) -> Round {
    Round::new(round.epoch(), parent_view)
}

impl ApplicationShared {
    /// Canonicalize parent and build a block on top of it.
    ///
    /// recover the direct parent's canonical Finalization parent-proof
    /// record from marshal's durable finalization archive when the in-process
    /// selection store missed it (restart / late-join / brief finalization lag).
    ///
    /// `get_finalization` is a LOCAL archive read. It never triggers a network
    /// fetch, so this cannot block consensus on a peer. The archive is the same
    /// durable store that marshal repopulates during sync. The rebuilt record
    /// mirrors the live [`FinalizationActor`](crate::finalization::actor) writer
    /// field-for-field, so the proposer's Phase 1 metadata stays canonical and
    /// every validator accepts it. The `record_builder_parity` test pins this
    /// parity. Returns `None` when one of these is true:
    /// - the archive has no finalization for `parent_height`.
    /// - the recovered finalization does not finalize this exact parent.
    /// - the finalized epoch's committee scheme / ordered addresses are not
    ///   registered.
    // `pub(crate)` for the regression test in `handler_tests` (a sibling
    // module). That test exercises the selection-store-miss recovery branch directly.
    pub(crate) async fn recover_parent_proof_from_marshal(
        &self,
        parent_proof_key: crate::finalization::parent_cert_store::CertifiedParentProofKey,
        parent_height: u64,
    ) -> Option<crate::finalization::parent_cert_store::CertifiedParentProofRecord> {
        use commonware_codec::Encode as _;

        let finalization = self
            .marshal_mailbox
            .get_finalization(Height::new(parent_height))
            .await?;
        // Hash-exact: the recovered finalization must finalize THIS parent.
        if finalization.proposal.payload.0 != parent_proof_key.block_hash {
            return None;
        }
        let epoch = finalization.proposal.round.epoch();
        let scheme = self.certificate_scheme_provider.scoped(epoch)?;
        let ordered = self.committee_provider.ordered_committee(epoch)?;
        let encoded: alloy_primitives::Bytes = finalization.encode().into();
        match crate::finalization::resolver::build_recovered_finalization_record(
            crate::finalization::resolver::RecoveredFinalizedBlock::from_proposal(
                parent_height,
                &finalization.proposal,
            ),
            crate::finalization::resolver::RecoveredFinalizationMaterial {
                ordered_committee: ordered.as_ref(),
                certificate: &finalization.certificate,
                encoded_certificate: encoded,
                scheme: scheme.as_ref(),
            },
        ) {
            Ok(record) => Some(record),
            Err(error) => {
                // Encode-invariant violation on the marshal-recovery path: no
                // canonical record can be produced, so recovery is unavailable.
                // This is deterministic and never a wrong proof. Logged, not fatal.
                tracing::warn!(
                    target: "outbe::application",
                    epoch = epoch.get(),
                    parent_height,
                    %error,
                    "marshal-recovered finalization record build failed; \
                     no Phase 1 recovery record available"
                );
                None
            }
        }
    }

    pub(super) async fn select_parent_proof_for_proposal(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        round: Round,
        parent_digest: Digest,
        parent_height: Height,
        parent_proof_key: Option<CertifiedParentProofKey>,
    ) -> ParentProofLookup {
        if parent_height.get() == 0 {
            return ParentProofLookup::NoProofNeeded;
        }
        let Some(parent_proof_key) = parent_proof_key else {
            warn!(
                %round,
                parent = %parent_digest.0,
                parent_height = parent_height.get(),
                "non-genesis proposal missing exact parent proof key"
            );
            crate::metrics::record_parent_cert_missing();
            crate::metrics::record_parent_proof_unavailable_forfeit();
            crate::metrics::record_phase1_parent_proof_unavailable();
            return ParentProofLookup::Unavailable;
        };
        match self
            .finalization_selector
            .select_direct_parent_proof_by_key_with_wait(
                clock,
                parent_proof_key,
                parent_height.get(),
                crate::finalization::selection::PHASE1_FINALIZATION_WAIT_DEFAULT,
            )
            .await
        {
            Some(record) => ParentProofLookup::Found(record),
            None => {
                // The in-process selection store missed the direct parent's proof
                // (post-restart, late-joining validator, or brief finalization lag).
                // But marshal's DURABLE finalization archive may still hold the
                // parent's finalization locally. Recover it and rebuild the
                // canonical Finalization parent-proof record before forfeiting the
                // slot.
                match self
                    .recover_parent_proof_from_marshal(parent_proof_key, parent_height.get())
                    .await
                {
                    Some(record) => {
                        crate::metrics::record_parent_proof_recovered_from_marshal();
                        info!(
                            %round,
                            parent = %parent_digest.0,
                            parent_height = parent_height.get(),
                            "recovered direct-parent proof from marshal archive; slot not forfeited"
                        );
                        ParentProofLookup::Found(record)
                    }
                    None => {
                        crate::metrics::record_parent_cert_missing();
                        crate::metrics::record_parent_proof_unavailable_forfeit();
                        crate::metrics::record_phase1_parent_proof_unavailable();
                        ParentProofLookup::Unavailable
                    }
                }
            }
        }
    }
}
