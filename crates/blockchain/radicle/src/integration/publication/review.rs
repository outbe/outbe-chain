//! Reconstruct the approved native Review package before issuing a publication attestation.

use super::{
    CheckpointOwnerSnapshot, NativeSubject, ObjectCommitment, ObjectKind, OwnerRef,
    PublicationManifest, RefUpdate,
};
use crate::{
    integration::{
        review::{object_closure, prepare_patch_review_with_objects, Error as PreparationError},
        signing::{
            accepted::AcceptedPublicationVerifier, witness::RepositoryWitnessBundle,
            RepositorySigningMaterial,
        },
    },
    manager::{FinalizedPatchApproval, FinalizedPatchSigningState, FinalizedPublicationReceipt},
};
use commonware_cryptography::{Hasher as _, Sha256};
use outbe_coderegistry::{payload::PatchVotePayloadV1, PatchApprovalRecord};
use outbe_primitives::radicle_publication::{PublicationAction, PublicationBody};
use radicle::{
    cob::patch,
    crypto::{BlsEnvelope, RepositorySubject},
    git,
    storage::{git::Repository, refs::SIGREFS_BRANCH, WriteRepository as _},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Merge(#[from] crate::integration::merge::Error),
    #[error("Review package does not match current finalized permission or checkpoint")]
    Context,
    #[error("Review package differs from the reconstructed native objects and refs")]
    Package,
    #[error(transparent)]
    Manifest(#[from] super::Error),
    #[error(transparent)]
    Preparation(#[from] PreparationError),
    #[error(transparent)]
    Signing(#[from] crate::integration::signing::Error),
    #[error(transparent)]
    Native(#[from] radicle::crypto::repository::Error),
    #[error(transparent)]
    Git(#[from] git::raw::Error),
}

/// Constructed only after native reconstruction, complete object and q-witness checks.
pub struct VerifiedReviewPublication {
    body: PublicationBody,
    manifest: PublicationManifest,
    approval: PatchApprovalRecord,
    previous: FinalizedPublicationReceipt,
    witness_hash: alloy_primitives::B256,
}

impl VerifiedReviewPublication {
    pub(crate) fn witness_hash(&self) -> alloy_primitives::B256 {
        self.witness_hash
    }
    pub fn body(&self) -> &PublicationBody {
        &self.body
    }
    pub fn manifest(&self) -> &PublicationManifest {
        &self.manifest
    }
    /// Reread local finalized state and material before each publication attestation.
    pub fn authorize_current(
        &self,
        state: &FinalizedPatchSigningState,
        material: &RepositorySigningMaterial,
    ) -> Result<(), Error> {
        let mut expected = review_body(state, &self.previous, &self.approval, material)?;
        expected.package_manifest_hash = self.manifest.hash()?;
        if expected != self.body {
            return Err(Error::Context);
        }
        Ok(())
    }
}

pub(super) struct PatchPublicationInput<'a> {
    pub repo: &'a Repository,
    pub state: &'a FinalizedPatchSigningState,
    pub snapshot: &'a CheckpointOwnerSnapshot,
    pub material: &'a RepositorySigningMaterial,
}

macro_rules! with_legacy_signature {
    ($($item:item)*) => {
        $($item)*
    };
}

pub(super) use with_legacy_signature;

with_legacy_signature! {
    /// Run off the consensus executor on a blocking worker. Only immutable objects are written;
    /// refs remain at the accepted checkpoint until its successor is finalized.
    pub fn prepare_review_publication(
        repo: &Repository,
        state: &FinalizedPatchSigningState,
        snapshot: &CheckpointOwnerSnapshot,
        material: &RepositorySigningMaterial,
        signatures: [&BlsEnvelope; 2],
        witness: &RepositoryWitnessBundle,
    ) -> Result<VerifiedReviewPublication, Error> {
        prepare_review_publication_with(
            PatchPublicationInput {
                repo,
                state,
                snapshot,
                material,
            },
            signatures,
            witness,
        )
    }

    /// Reconstruct from trusted inputs, never from the peer's declared native subjects or refs.
    pub fn verify_review_publication(
        repo: &Repository,
        state: &FinalizedPatchSigningState,
        snapshot: &CheckpointOwnerSnapshot,
        material: &RepositorySigningMaterial,
        body: &PublicationBody,
        manifest: &PublicationManifest,
        witness: &RepositoryWitnessBundle,
    ) -> Result<VerifiedReviewPublication, Error> {
        verify_review_publication_with(
            PatchPublicationInput {
                repo,
                state,
                snapshot,
                material,
            },
            (body, manifest),
            witness,
        )
    }
}

fn prepare_review_publication_with(
    input: PatchPublicationInput<'_>,
    signatures: [&BlsEnvelope; 2],
    witness: &RepositoryWitnessBundle,
) -> Result<VerifiedReviewPublication, Error> {
    let PatchPublicationInput {
        repo,
        state,
        snapshot,
        material,
    } = input;
    let mut body = review_body(
        state,
        snapshot.checkpoint(),
        &state.approval().approval,
        material,
    )?;
    let context = material.review_context(body.previous_receipt_hash.0);
    let manifest = reconstruct_review_with(
        ReconstructInput {
            repo,
            approval: state.approval(),
            snapshot,
            context: &context,
        },
        signatures,
        |actual| witness.verify(material, actual),
    )?;
    body.package_manifest_hash = manifest.hash()?;
    manifest.bind_receipt(&body)?;
    Ok(VerifiedReviewPublication {
        body,
        manifest,
        approval: state.approval().approval.clone(),
        previous: snapshot.checkpoint().clone(),
        witness_hash: Sha256::hash(&[&witness.to_bytes()?]).0.into(),
    })
}

fn verify_review_publication_with(
    input: PatchPublicationInput<'_>,
    candidate: (&PublicationBody, &PublicationManifest),
    witness: &RepositoryWitnessBundle,
) -> Result<VerifiedReviewPublication, Error> {
    let (body, manifest) = candidate;
    manifest.bind_receipt(body)?;
    if manifest.subjects().len() != 2 {
        return Err(Error::Package);
    }
    let signatures = [
        BlsEnvelope::from_hex(&manifest.subjects()[0].envelope)?,
        BlsEnvelope::from_hex(&manifest.subjects()[1].envelope)?,
    ];
    let expected =
        prepare_review_publication_with(input, [&signatures[0], &signatures[1]], witness)?;
    if expected.body != *body || expected.manifest != *manifest {
        return Err(Error::Package);
    }
    Ok(expected)
}

fn review_body(
    state: &FinalizedPatchSigningState,
    previous: &FinalizedPublicationReceipt,
    approval: &PatchApprovalRecord,
    material: &RepositorySigningMaterial,
) -> Result<PublicationBody, Error> {
    patch_body(
        state,
        previous,
        approval,
        material,
        PublicationAction::Review,
    )
}

pub(super) fn patch_body(
    state: &FinalizedPatchSigningState,
    previous: &FinalizedPublicationReceipt,
    approval: &PatchApprovalRecord,
    material: &RepositorySigningMaterial,
    action: PublicationAction,
) -> Result<PublicationBody, Error> {
    let stage = match action {
        PublicationAction::Review => outbe_coderegistry::schema::PATCH_EXECUTION_PENDING,
        PublicationAction::Merge => outbe_coderegistry::schema::PATCH_EXECUTION_REVIEWED,
        _ => return Err(Error::Context),
    };
    let context = material.review_context(previous.receipt().receipt_hash.0);
    let authority = state.authority();
    let material_body = &state.material().receipt().body;
    let previous_body = &previous.receipt().body;
    if (
        authority,
        &state.approval().approval,
        state.stage(),
        state.checkpoint().receipt(),
        context.chain_id,
        context.genesis_hash,
        context.epoch,
        context.material_version,
    ) != (
        material.authority(),
        approval,
        stage,
        previous.receipt(),
        authority.chain_id(),
        authority.genesis_hash().0,
        authority.epoch(),
        authority.committee().vrf_material_version,
    ) {
        return Err(Error::Context);
    }
    if (
        material.material_hash(),
        material.players_commitment(),
        previous_body.epoch,
        previous_body.material_version,
        previous_body.material_hash.0,
        previous_body.players_commitment.0,
    ) != (
        material_body.material_hash.0,
        material_body.players_commitment.0,
        context.epoch,
        context.material_version,
        context.material_hash,
        material.players_commitment(),
    ) {
        return Err(Error::Context);
    }
    let decision =
        PatchVotePayloadV1::decode(approval.payload.as_bytes()).map_err(|_| Error::Context)?;
    let mut body = previous.receipt().body.clone();
    if decision.repo_id != body.repo_id {
        return Err(Error::Context);
    }
    body.sequence = body.sequence.checked_add(1).ok_or(Error::Context)?;
    body.previous_receipt_hash = previous.receipt().receipt_hash;
    body.action = action;
    body.proposal_id = approval.proposal_id;
    body.patch_id = decision.patch_id;
    body.revision_id = decision.revision_id;
    body.expected_target_head = decision.expected_target_head;
    body.finalized_anchor_height = state.anchor().number;
    body.finalized_anchor_hash = state.anchor().hash;
    body.package_manifest_hash = Default::default();
    Ok(body)
}

pub(super) struct ReconstructInput<'a> {
    pub repo: &'a Repository,
    pub approval: &'a FinalizedPatchApproval,
    pub snapshot: &'a CheckpointOwnerSnapshot,
    pub context: &'a crate::integration::review::ReviewSigningContext,
}

pub(super) fn unpinned_object_commitments(
    repo: &Repository,
    roots: Vec<git::Oid>,
    pinned: &std::collections::BTreeSet<git::Oid>,
) -> Result<Vec<ObjectCommitment>, Error> {
    let new_objects = object_closure(repo.raw(), roots, false)?;
    let odb = repo.raw().odb()?;
    new_objects
        .difference(pinned)
        .map(|oid| {
            let object = odb.read((*oid).into())?;
            let kind = match object.kind() {
                git::raw::ObjectType::Blob => ObjectKind::Blob,
                git::raw::ObjectType::Tree => ObjectKind::Tree,
                git::raw::ObjectType::Commit => ObjectKind::Commit,
                _ => return Err(Error::Package),
            };
            Ok(ObjectCommitment {
                kind,
                oid: oid.to_string(),
                sha256: hex::encode(Sha256::hash(&[object.data()]).0),
            })
        })
        .collect()
}

pub(super) fn verify_accepted_patch_context(
    approval: &FinalizedPatchApproval,
    previous: &CheckpointOwnerSnapshot,
    verifier: &AcceptedPublicationVerifier,
    action: PublicationAction,
) -> Result<[BlsEnvelope; 2], Error> {
    let receipt = verifier.receipt();
    let body = &receipt.receipt().body;
    let before = previous.checkpoint().receipt();
    let decision = PatchVotePayloadV1::decode(approval.approval.payload.as_bytes())
        .map_err(|_| Error::Context)?;
    if body.action != action
        || verifier.manifest().subjects().len() != 2
        || body.proposal_id != approval.approval.proposal_id
    {
        return Err(Error::Context);
    }
    if (
        body.repo_id,
        body.patch_id,
        body.revision_id,
        body.expected_target_head,
    ) != (
        decision.repo_id,
        decision.patch_id,
        decision.revision_id,
        decision.expected_target_head,
    ) {
        return Err(Error::Context);
    }
    let expected_anchor = approval
        .approval
        .approved_height
        .max(before.accepted_height);
    if approval.approval.approved_height > receipt.receipt().accepted_height
        || body.finalized_anchor_height != expected_anchor
    {
        return Err(Error::Context);
    }
    if before.body.sequence.checked_add(1) != Some(body.sequence)
        || body.previous_receipt_hash != before.receipt_hash
    {
        return Err(Error::Context);
    }
    if (
        body.chain_id,
        body.genesis_hash,
        body.owner_bls_public,
        body.descriptor_hash,
        body.players_commitment,
    ) != (
        before.body.chain_id,
        before.body.genesis_hash,
        before.body.owner_bls_public,
        before.body.descriptor_hash,
        before.body.players_commitment,
    ) {
        return Err(Error::Context);
    }
    Ok([
        BlsEnvelope::from_hex(&verifier.manifest().subjects()[0].envelope)?,
        BlsEnvelope::from_hex(&verifier.manifest().subjects()[1].envelope)?,
    ])
}

with_legacy_signature! {
    pub(super) fn reconstruct_review(
        repo: &Repository,
        approval: &FinalizedPatchApproval,
        snapshot: &CheckpointOwnerSnapshot,
        context: &crate::integration::review::ReviewSigningContext,
        signatures: [&BlsEnvelope; 2],
        verify: impl FnOnce(
            &[(RepositorySubject, BlsEnvelope)],
        ) -> Result<(), crate::integration::signing::Error>,
    ) -> Result<PublicationManifest, Error> {
        reconstruct_review_with(
            ReconstructInput {
                repo,
                approval,
                snapshot,
                context,
            },
            signatures,
            verify,
        )
    }
}

pub(super) fn reconstruct_review_with(
    input: ReconstructInput<'_>,
    signatures: [&BlsEnvelope; 2],
    verify: impl FnOnce(
        &[(RepositorySubject, BlsEnvelope)],
    ) -> Result<(), crate::integration::signing::Error>,
) -> Result<PublicationManifest, Error> {
    let ReconstructInput {
        repo,
        approval,
        snapshot,
        context,
    } = input;
    let (job, pinned) = prepare_patch_review_with_objects(repo, approval, snapshot, context)?;
    let owner = job.owner();
    let subject = job.subject().clone();
    let (entry, refs) = job.attach(repo, signatures[0])?;
    let refs_subject = refs.subject().clone();
    verify(&[
        (subject, signatures[0].clone()),
        (refs_subject, signatures[1].clone()),
    ])?;
    let sigrefs = refs.attach(repo.raw(), signatures[1].clone())?.oid();
    let objects = unpinned_object_commitments(repo, vec![entry.id, sigrefs], &pinned)?;
    let patch_id = git::Oid::from_sha1(
        PatchVotePayloadV1::decode(approval.approval.payload.as_bytes())
            .map_err(|_| Error::Context)?
            .patch_id
            .0,
    )
    .into();
    let name =
        git::refs::storage::cob(&owner, &patch::TYPENAME, &patch_id).strip_namespace_recursive();
    let old = snapshot
        .owner_refs()
        .get(&name)
        .unwrap_or(git::Oid::ZERO_SHA1);
    let mut desired = snapshot.owner_refs().clone();
    desired.insert(name.to_ref_string(), entry.id);
    let manifest = PublicationManifest::new(
        objects,
        vec![
            RefUpdate {
                name: name.to_string(),
                old: old.to_string(),
                new: entry.id.to_string(),
            },
            RefUpdate {
                name: SIGREFS_BRANCH.to_string(),
                old: snapshot.sigrefs().to_string(),
                new: sigrefs.to_string(),
            },
        ],
        signatures
            .into_iter()
            .enumerate()
            .map(|(ordinal, envelope)| NativeSubject {
                ordinal: ordinal as u8,
                envelope: envelope.to_hex(),
            })
            .collect(),
        desired
            .iter()
            .map(|(name, oid)| OwnerRef {
                name: name.to_string(),
                oid: oid.to_string(),
            })
            .collect(),
    )?;
    Ok(manifest)
}

/// This proof authorizes only the exact accepted package, never a new signing job.
pub struct AcceptedReviewPublication {
    receipt: FinalizedPublicationReceipt,
    manifest: PublicationManifest,
}

impl AcceptedReviewPublication {
    /// Promote only this accepted Review; the target branch remains at its signed snapshot.
    pub fn install(&self, repo: &Repository) -> Result<(), super::Error> {
        super::install_owner_refs(repo, &self.receipt.receipt().body, &self.manifest)
    }
    pub fn receipt(&self) -> &FinalizedPublicationReceipt {
        &self.receipt
    }
    pub fn manifest(&self) -> &PublicationManifest {
        &self.manifest
    }
}

/// Reconstruct exact accepted history without requiring the retired material to be current.
pub fn verify_accepted_review_publication(
    repo: &Repository,
    approval: &FinalizedPatchApproval,
    previous: &CheckpointOwnerSnapshot,
    verifier: &AcceptedPublicationVerifier,
) -> Result<AcceptedReviewPublication, Error> {
    let receipt = verifier.receipt();
    let signatures =
        verify_accepted_patch_context(approval, previous, verifier, PublicationAction::Review)?;
    let expected = reconstruct_review_with(
        ReconstructInput {
            repo,
            approval,
            snapshot: previous,
            context: &verifier.review_context(),
        },
        [&signatures[0], &signatures[1]],
        |actual| verifier.verify_native(actual),
    )?;
    if expected != *verifier.manifest() {
        return Err(Error::Package);
    }
    Ok(AcceptedReviewPublication {
        receipt: receipt.clone(),
        manifest: expected,
    })
}
