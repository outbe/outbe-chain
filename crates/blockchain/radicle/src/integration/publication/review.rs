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
use outbe_primitives::radicle_publication::{PublicationAction, PublicationBody};
use outbe_radicleregistry::{payload::PatchVotePayloadV1, PatchApprovalRecord};
use radicle::{
    cob::patch,
    crypto::{BlsEnvelope, RepositorySubject},
    git,
    storage::{git::Repository, refs::SIGREFS_BRANCH, WriteRepository as _},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
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
    let mut body = review_body(
        state,
        snapshot.checkpoint(),
        &state.approval().approval,
        material,
    )?;
    let context = material.review_context(body.previous_receipt_hash.0);
    let manifest = reconstruct_review(
        repo,
        state.approval(),
        snapshot,
        &context,
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
    manifest.bind_receipt(body)?;
    if manifest.subjects().len() != 2 {
        return Err(Error::Package);
    }
    let signatures = [
        BlsEnvelope::from_hex(&manifest.subjects()[0].envelope)?,
        BlsEnvelope::from_hex(&manifest.subjects()[1].envelope)?,
    ];
    let expected = prepare_review_publication(
        repo,
        state,
        snapshot,
        material,
        [&signatures[0], &signatures[1]],
        witness,
    )?;
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
    let context = material.review_context(previous.receipt().receipt_hash.0);
    if state.authority() != material.authority()
        || &state.approval().approval != approval
        || state.stage() != outbe_radicleregistry::schema::PATCH_EXECUTION_PENDING
        || state.checkpoint().receipt() != previous.receipt()
        || context.chain_id != state.authority().chain_id()
        || context.genesis_hash != state.authority().genesis_hash().0
        || context.epoch != state.authority().epoch()
        || context.material_version != state.authority().committee().vrf_material_version
        || material.material_hash() != state.material().receipt().body.material_hash.0
        || material.players_commitment() != state.material().receipt().body.players_commitment.0
        || previous.receipt().body.epoch != context.epoch
        || previous.receipt().body.material_version != context.material_version
        || previous.receipt().body.material_hash.0 != context.material_hash
        || previous.receipt().body.players_commitment.0 != material.players_commitment()
    {
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
    body.action = PublicationAction::Review;
    body.proposal_id = approval.proposal_id;
    body.patch_id = decision.patch_id;
    body.revision_id = decision.revision_id;
    body.expected_target_head = decision.expected_target_head;
    body.finalized_anchor_height = state.anchor().number;
    body.finalized_anchor_hash = state.anchor().hash;
    body.package_manifest_hash = Default::default();
    Ok(body)
}

fn reconstruct_review(
    repo: &Repository,
    approval: &FinalizedPatchApproval,
    snapshot: &CheckpointOwnerSnapshot,
    context: &crate::integration::review::ReviewSigningContext,
    signatures: [&BlsEnvelope; 2],
    verify: impl FnOnce(
        &[(RepositorySubject, BlsEnvelope)],
    ) -> Result<(), crate::integration::signing::Error>,
) -> Result<PublicationManifest, Error> {
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
    let new_objects = object_closure(repo.raw(), vec![entry.id, sigrefs], false)?;
    let odb = repo.raw().odb()?;
    let objects = new_objects
        .difference(&pinned)
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
        .collect::<Result<Vec<_>, Error>>()?;
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
    witness: &RepositoryWitnessBundle,
) -> Result<AcceptedReviewPublication, Error> {
    let receipt = verifier.receipt();
    let body = &receipt.receipt().body;
    let before = previous.checkpoint().receipt();
    let decision = PatchVotePayloadV1::decode(approval.approval.payload.as_bytes())
        .map_err(|_| Error::Context)?;
    if body.action != PublicationAction::Review
        || verifier.manifest().subjects().len() != 2
        || body.proposal_id != approval.approval.proposal_id
        || body.repo_id != decision.repo_id
        || body.patch_id != decision.patch_id
        || body.revision_id != decision.revision_id
        || body.expected_target_head != decision.expected_target_head
        || approval.approval.approved_height > receipt.receipt().accepted_height
        || body.finalized_anchor_height
            != approval
                .approval
                .approved_height
                .max(before.accepted_height)
        || before.body.sequence.checked_add(1) != Some(body.sequence)
        || body.previous_receipt_hash != before.receipt_hash
        || body.chain_id != before.body.chain_id
        || body.genesis_hash != before.body.genesis_hash
        || body.owner_bls_public != before.body.owner_bls_public
        || body.descriptor_hash != before.body.descriptor_hash
        || body.players_commitment != before.body.players_commitment
    {
        return Err(Error::Context);
    }
    let signatures = [
        BlsEnvelope::from_hex(&verifier.manifest().subjects()[0].envelope)?,
        BlsEnvelope::from_hex(&verifier.manifest().subjects()[1].envelope)?,
    ];
    let expected = reconstruct_review(
        repo,
        approval,
        previous,
        &verifier.review_context(),
        [&signatures[0], &signatures[1]],
        |actual| witness.verify_accepted(verifier, actual),
    )?;
    if expected != *verifier.manifest() {
        return Err(Error::Package);
    }
    Ok(AcceptedReviewPublication {
        receipt: receipt.clone(),
        manifest: expected,
    })
}
