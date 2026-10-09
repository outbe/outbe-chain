//! Finalized-parent attestation validation surface.
//!
//! This module holds the determinism-critical consensus-metadata validation.
//! That validation previously shared `finalization::util` with generic leaf
//! helpers. Splitting it out keeps the BLS / committee / canonical identity
//! checks in one named module. `util` retains only pure leaf helpers (retry,
//! replay classification, header-artifact extraction, signer-bitmap fill).
//!
//! `validate_consensus_metadata` is the V2 structural + certificate predicate.
//! `validate_consensus_metadata_for_verify` is retained ONLY as a legacy test
//! fixture for `handler_tests.rs` cases that pre-date the V2 verifier. Production
//! runtime paths MUST NOT call it. They use
//! [`crate::proof::verify_v2_proof`] instead.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use alloy_primitives::{Address, B256};
use commonware_codec::Read as _;
use commonware_consensus::{
    simplex::types::Finalization,
    types::{Epoch, Height},
};
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_parallel::Sequential;
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;

use crate::{
    committee_provider::CommitteeProvider,
    digest::Digest,
    finalization::util::build_signer_bitmap,
    hybrid::{bls_batch_verification_rng, HybridScheme, HybridSchemeProvider},
};

/// Time budget for finalized-history metadata checks during verify and
/// proposer-side validate-before-include.
pub(crate) const METADATA_CANONICAL_LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

/// Single shared verdict enum for builder-side and verifier-side
/// finalized-parent attestation validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttestationVerdict {
    /// Attestation is absent. The block is valid, with no settlement.
    AcceptNone,
    /// Attestation present and valid. Embed (builder) / accept (verifier).
    AcceptValid,
    /// Bitmap / committee / missed-proposers structurally bad.
    RejectStructural,
    /// BLS certificate verification failed under the scoped scheme.
    RejectCertificate,
    /// Canonical identity or canonical missed-proposer calculation failed.
    RejectCanonicalIdentity,
    /// Local canonical information is not available yet.
    TransientUnavailable,
}

impl AttestationVerdict {
    pub fn is_accept(self) -> bool {
        matches!(self, AttestationVerdict::AcceptValid)
    }

    pub fn is_drain(self) -> bool {
        matches!(
            self,
            AttestationVerdict::RejectStructural
                | AttestationVerdict::RejectCertificate
                | AttestationVerdict::RejectCanonicalIdentity
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            AttestationVerdict::AcceptNone => "accept_none",
            AttestationVerdict::AcceptValid => "accept_valid",
            AttestationVerdict::RejectStructural => "reject_structural",
            AttestationVerdict::RejectCertificate => "reject_certificate",
            AttestationVerdict::RejectCanonicalIdentity => "reject_canonical_identity",
            AttestationVerdict::TransientUnavailable => "transient_unavailable",
        }
    }
}

pub struct AttestationValidationContext<'a> {
    pub certificate_scheme_provider: &'a HybridSchemeProvider<MinSig>,
    pub committee_provider: &'a CommitteeProvider,
    pub marshal_mailbox: &'a crate::marshal_types::MarshalMailbox,
    pub proposed_block_number: u64,
}

// `validate_finalized_parent_attestation` was the V1
// async certificate-validation predicate. The proposer-side exact-parent
// wait and `handle_verify` used it. Both call sites are removed. The
// proposer reads the proof store directly. `handle_verify` does not decode or
// verify the carried BLS certificate in its prechecks. The EVM-side V2
// verifier does that during execution verification. The function is deleted
// to prevent accidental reintroduction of the BLS-on-verify path.
//
// `validate_consensus_metadata_for_verify` below is retained ONLY as a
// test fixture for legacy `handler_tests.rs` cases that pre-date the V2
// verifier. Production runtime paths MUST NOT call it. They use
// `crate::proof::verify_v2_proof` instead.

pub async fn validate_consensus_metadata_for_verify(
    clock: &impl commonware_runtime::Clock,
    actual: Option<&CertifiedParentAccountingMetadata>,
    ctx: &AttestationValidationContext<'_>,
) -> AttestationVerdict {
    let Some(actual) = actual else {
        return AttestationVerdict::AcceptNone;
    };

    match validate_present_metadata_for_verify(clock, actual, ctx).await {
        Ok(()) => AttestationVerdict::AcceptValid,
        Err(verdict) => verdict,
    }
}

async fn validate_present_metadata_for_verify(
    clock: &impl commonware_runtime::Clock,
    actual: &CertifiedParentAccountingMetadata,
    ctx: &AttestationValidationContext<'_>,
) -> Result<(), AttestationVerdict> {
    resolve_metadata_verify_scope(actual, ctx)?;

    let digest = Digest(actual.finalized_block_hash);
    // The marshal lookup future borrows `&digest`, so it is not `'static` and
    // cannot use `Clock::timeout`. Inline the same race `Clock::timeout` uses:
    // a biased select between the lookup and a runtime-agnostic sleep.
    let info_lookup = ctx.marshal_mailbox.get_info(&digest);
    let timeout = clock.sleep(METADATA_CANONICAL_LOOKUP_TIMEOUT);
    let mut info_lookup = std::pin::pin!(info_lookup);
    let mut timeout = std::pin::pin!(timeout);
    let lookup = commonware_macros::select! {
        result = &mut info_lookup => Some(result),
        _ = &mut timeout => None,
    };
    let canonical_identity = match lookup {
        Some(Some((height, canonical_digest))) => {
            height == Height::new(actual.finalized_block_number) && canonical_digest == digest
        }
        Some(None) | None => return Err(AttestationVerdict::TransientUnavailable),
    };
    if !canonical_identity {
        return Err(AttestationVerdict::RejectCanonicalIdentity);
    }

    // The structural predicate already enforces the V2 empty list.
    Ok(())
}

type MetadataVerifyScope = (Arc<Vec<Address>>, Arc<HybridScheme<MinSig>>);

fn resolve_metadata_verify_scope(
    actual: &CertifiedParentAccountingMetadata,
    ctx: &AttestationValidationContext<'_>,
) -> Result<MetadataVerifyScope, AttestationVerdict> {
    let certificate_verdict = validate_consensus_metadata(
        Some(actual),
        ctx.certificate_scheme_provider,
        ctx.committee_provider,
    );
    if certificate_verdict != AttestationVerdict::AcceptValid {
        return Err(certificate_verdict);
    }

    if actual.finalized_block_number >= ctx.proposed_block_number {
        return Err(AttestationVerdict::RejectStructural);
    }

    let epoch = Epoch::new(actual.finalized_epoch);
    let expected_committee = ctx
        .committee_provider
        .ordered_committee(epoch)
        .ok_or(AttestationVerdict::RejectStructural)?;
    let scheme = ctx
        .certificate_scheme_provider
        .scoped(epoch)
        .ok_or(AttestationVerdict::RejectCertificate)?;

    Ok((expected_committee, scheme))
}

pub(crate) fn validate_consensus_metadata(
    actual: Option<&CertifiedParentAccountingMetadata>,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    committee_provider: &CommitteeProvider,
) -> AttestationVerdict {
    let Some(actual) = actual else {
        return AttestationVerdict::AcceptNone;
    };
    match validate_present_consensus_metadata(
        actual,
        certificate_scheme_provider,
        committee_provider,
    ) {
        Ok(()) => AttestationVerdict::AcceptValid,
        Err(verdict) => verdict,
    }
}

fn validate_present_consensus_metadata(
    actual: &CertifiedParentAccountingMetadata,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    committee_provider: &CommitteeProvider,
) -> Result<(), AttestationVerdict> {
    if actual.finalized_block_number == 0 || actual.finalized_block_hash == B256::ZERO {
        return Err(AttestationVerdict::RejectStructural);
    }
    let epoch = Epoch::new(actual.finalized_epoch);
    let expected_committee = validate_metadata_committee(actual, epoch, committee_provider)?;
    let finalization = decode_metadata_finalization(actual, expected_committee.len())?;
    let scheme = certificate_scheme_provider
        .scoped(epoch)
        .ok_or(AttestationVerdict::RejectCertificate)?;
    if !metadata_proposal_is_bound(&finalization, actual, epoch) {
        return Err(AttestationVerdict::RejectStructural);
    }
    let mut rng = bls_batch_verification_rng();
    if !finalization.verify(&mut rng, scheme.as_ref(), &Sequential) {
        return Err(AttestationVerdict::RejectCertificate);
    }

    // The V2 signer bitmap is the certificate's own bitmap. There is no
    // supplemental finalize-vote reconciliation. The V1
    // `build_signer_bitmap_with_finalize_votes` helper is dropped.
    let expected_bitmap = build_signer_bitmap(&finalization.certificate, expected_committee.len());
    if expected_bitmap == actual.signer_bitmap {
        Ok(())
    } else {
        Err(AttestationVerdict::RejectCertificate)
    }
}

fn validate_metadata_committee(
    actual: &CertifiedParentAccountingMetadata,
    epoch: Epoch,
    committee_provider: &CommitteeProvider,
) -> Result<Arc<Vec<Address>>, AttestationVerdict> {
    let expected_committee = committee_provider
        .ordered_committee(epoch)
        .ok_or(AttestationVerdict::RejectStructural)?;
    if expected_committee.as_ref() != &actual.ordered_committee {
        return Err(AttestationVerdict::RejectStructural);
    }
    if actual.signer_bitmap.len() != expected_committee.len() {
        return Err(AttestationVerdict::RejectStructural);
    }
    if actual.signer_bitmap.iter().any(|byte| *byte > 1) {
        return Err(AttestationVerdict::RejectStructural);
    }
    let committee_set: BTreeSet<_> = expected_committee.iter().copied().collect();
    // The V2 contract requires `missed_proposers` to be empty. If any
    // event is present, it must reference a committee member. This is a
    // defensive structural check. The V2 verifier enforces emptiness upstream.
    if actual
        .missed_proposers
        .iter()
        .any(|ev| !committee_set.contains(&ev.validator))
    {
        return Err(AttestationVerdict::RejectStructural);
    }
    Ok(expected_committee)
}

fn decode_metadata_finalization(
    actual: &CertifiedParentAccountingMetadata,
    member_count: usize,
) -> Result<Finalization<HybridScheme<MinSig>, Digest>, AttestationVerdict> {
    let mut proof_reader = actual.proof.as_ref();
    let finalization =
        Finalization::<HybridScheme<MinSig>, Digest>::read_cfg(&mut proof_reader, &member_count)
            .map_err(|_| AttestationVerdict::RejectCertificate)?;
    if !proof_reader.is_empty() {
        return Err(AttestationVerdict::RejectCertificate);
    }
    Ok(finalization)
}

fn metadata_proposal_is_bound(
    finalization: &Finalization<HybridScheme<MinSig>, Digest>,
    actual: &CertifiedParentAccountingMetadata,
    epoch: Epoch,
) -> bool {
    let proposal = &finalization.proposal;
    proposal.round.epoch() == epoch
        && proposal.round.view().get() == actual.finalized_view
        && proposal.parent.get() == actual.parent_view
        && proposal.payload.0 == actual.finalized_block_hash
}
