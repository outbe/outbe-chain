//! The existing native Merge action and its complete signed refs form one publication.
use super::{
    review::{
        patch_body, unpinned_object_commitments, verify_accepted_patch_context,
        with_legacy_signature, AcceptedReviewPublication, Error, PatchPublicationInput,
        ReconstructInput,
    },
    CheckpointOwnerSnapshot, NativeSubject, OwnerRef, PublicationManifest, RefUpdate,
};
use crate::{
    integration::{
        merge::prepare_patch_merge_from_checkpoint,
        review::object_closure,
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
    crypto::{BlsEnvelope, RepositorySubject},
    git,
    storage::{git::Repository, refs::SIGREFS_BRANCH, WriteRepository as _},
};

pub struct VerifiedMergePublication {
    body: PublicationBody,
    manifest: PublicationManifest,
    approval: PatchApprovalRecord,
    previous: FinalizedPublicationReceipt,
    witness_hash: alloy_primitives::B256,
}
impl VerifiedMergePublication {
    pub fn body(&self) -> &PublicationBody {
        &self.body
    }
    pub fn manifest(&self) -> &PublicationManifest {
        &self.manifest
    }
    pub(crate) fn witness_hash(&self) -> alloy_primitives::B256 {
        self.witness_hash
    }
    pub fn authorize_current(
        &self,
        state: &FinalizedPatchSigningState,
        material: &RepositorySigningMaterial,
    ) -> Result<(), Error> {
        let mut expected = patch_body(
            state,
            &self.previous,
            &self.approval,
            material,
            PublicationAction::Merge,
        )?;
        expected.package_manifest_hash = self.manifest.hash()?;
        if expected != self.body {
            return Err(Error::Context);
        }
        Ok(())
    }
}

with_legacy_signature! {
    pub fn prepare_merge_publication(
        repo: &Repository,
        state: &FinalizedPatchSigningState,
        snapshot: &CheckpointOwnerSnapshot,
        accepted_review: &AcceptedReviewPublication,
        material: &RepositorySigningMaterial,
        signatures: [&BlsEnvelope; 2],
        witness: &RepositoryWitnessBundle,
    ) -> Result<VerifiedMergePublication, Error> {
        prepare_merge_publication_with(
            PatchPublicationInput {
                repo,
                state,
                snapshot,
                material,
            },
            accepted_review,
            signatures,
            witness,
        )
    }

    pub fn verify_merge_publication(
        repo: &Repository,
        state: &FinalizedPatchSigningState,
        snapshot: &CheckpointOwnerSnapshot,
        accepted_review: &AcceptedReviewPublication,
        material: &RepositorySigningMaterial,
        candidate: (&PublicationBody, &PublicationManifest),
        witness: &RepositoryWitnessBundle,
    ) -> Result<VerifiedMergePublication, Error> {
        verify_merge_publication_with(
            PatchPublicationInput {
                repo,
                state,
                snapshot,
                material,
            },
            accepted_review,
            candidate,
            witness,
        )
    }
}

fn prepare_merge_publication_with(
    input: PatchPublicationInput<'_>,
    accepted_review: &AcceptedReviewPublication,
    signatures: [&BlsEnvelope; 2],
    witness: &RepositoryWitnessBundle,
) -> Result<VerifiedMergePublication, Error> {
    let PatchPublicationInput {
        repo,
        state,
        snapshot,
        material,
    } = input;
    let mut body = patch_body(
        state,
        snapshot.checkpoint(),
        &state.approval().approval,
        material,
        PublicationAction::Merge,
    )?;
    let context = material.review_context(body.previous_receipt_hash.0);
    let manifest = reconstruct_with(
        ReconstructInput {
            repo,
            approval: state.approval(),
            snapshot,
            context: &context,
        },
        accepted_review,
        signatures,
        |actual| witness.verify(material, actual),
    )?;
    body.package_manifest_hash = manifest.hash()?;
    manifest.bind_receipt(&body)?;
    Ok(VerifiedMergePublication {
        body,
        manifest,
        approval: state.approval().approval.clone(),
        previous: snapshot.checkpoint().clone(),
        witness_hash: Sha256::hash(&[&witness.to_bytes()?]).0.into(),
    })
}

fn verify_merge_publication_with(
    input: PatchPublicationInput<'_>,
    accepted_review: &AcceptedReviewPublication,
    candidate: (&PublicationBody, &PublicationManifest),
    witness: &RepositoryWitnessBundle,
) -> Result<VerifiedMergePublication, Error> {
    let (body, manifest) = candidate;
    manifest.bind_receipt(body)?;
    if manifest.subjects().len() != 2 {
        return Err(Error::Package);
    }
    let signatures = [
        BlsEnvelope::from_hex(&manifest.subjects()[0].envelope)?,
        BlsEnvelope::from_hex(&manifest.subjects()[1].envelope)?,
    ];
    let expected = prepare_merge_publication_with(
        input,
        accepted_review,
        [&signatures[0], &signatures[1]],
        witness,
    )?;
    if expected.body != *body || expected.manifest != *manifest {
        return Err(Error::Package);
    }
    Ok(expected)
}

with_legacy_signature! {
    pub(super) fn reconstruct(
        repo: &Repository,
        approval: &FinalizedPatchApproval,
        snapshot: &CheckpointOwnerSnapshot,
        accepted_review: &AcceptedReviewPublication,
        context: &crate::integration::review::ReviewSigningContext,
        signatures: [&BlsEnvelope; 2],
        verify: impl FnOnce(
            &[(RepositorySubject, BlsEnvelope)],
        ) -> Result<(), crate::integration::signing::Error>,
    ) -> Result<PublicationManifest, Error> {
        reconstruct_with(
            ReconstructInput {
                repo,
                approval,
                snapshot,
                context,
            },
            accepted_review,
            signatures,
            verify,
        )
    }
}

fn reconstruct_with(
    input: ReconstructInput<'_>,
    accepted_review: &AcceptedReviewPublication,
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
    let decision = PatchVotePayloadV1::decode(approval.approval.payload.as_bytes())
        .map_err(|_| Error::Context)?;
    let mut old_roots: Vec<_> = snapshot.owner_refs().iter().map(|(_, oid)| *oid).collect();
    old_roots.extend([
        snapshot.sigrefs(),
        git::Oid::from_sha1(decision.revision_id.0),
    ]);
    let pinned = object_closure(repo.raw(), old_roots, true)?;
    let job =
        prepare_patch_merge_from_checkpoint(repo, approval, snapshot, accepted_review, context)?;
    let result = job.result();
    let owner = job.owner();
    let subject = job.subject().clone();
    let (entry, refs) = job.attach(repo, signatures[0])?;
    let refs_subject = refs.subject().clone();
    verify(&[
        (subject, signatures[0].clone()),
        (refs_subject, signatures[1].clone()),
    ])?;
    let sigrefs = refs.attach(repo.raw(), signatures[1].clone())?.oid();
    let objects = unpinned_object_commitments(repo, vec![entry.id, sigrefs, result], &pinned)?;
    let patch = git::Oid::from_sha1(decision.patch_id.0).into();
    let name = git::refs::storage::cob(&owner, &radicle::cob::patch::TYPENAME, &patch)
        .strip_namespace_recursive();
    let main = git::fmt::qualified!("refs/heads/main");
    let old = snapshot.owner_refs().get(&name).ok_or(Error::Context)?;
    let old_main = snapshot.owner_refs().get(&main).ok_or(Error::Context)?;
    let mut desired = snapshot.owner_refs().clone();
    desired.insert(name.to_ref_string(), entry.id);
    desired.insert(main.to_ref_string(), result);
    let mut updates = vec![
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
    ];
    if result != old_main {
        updates.push(RefUpdate {
            name: main.to_string(),
            old: old_main.to_string(),
            new: result.to_string(),
        });
    }
    Ok(PublicationManifest::new(
        objects,
        updates,
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
    )?)
}

pub struct AcceptedMergePublication {
    receipt: FinalizedPublicationReceipt,
    manifest: PublicationManifest,
}
impl AcceptedMergePublication {
    pub fn receipt(&self) -> &FinalizedPublicationReceipt {
        &self.receipt
    }
    pub fn manifest(&self) -> &PublicationManifest {
        &self.manifest
    }
}

pub fn verify_accepted_merge_publication(
    repo: &Repository,
    approval: &FinalizedPatchApproval,
    previous: &CheckpointOwnerSnapshot,
    accepted_review: &AcceptedReviewPublication,
    verifier: &AcceptedPublicationVerifier,
) -> Result<AcceptedMergePublication, Error> {
    let receipt = verifier.receipt();
    let signatures =
        verify_accepted_patch_context(approval, previous, verifier, PublicationAction::Merge)?;
    let expected = reconstruct_with(
        ReconstructInput {
            repo,
            approval,
            snapshot: previous,
            context: &verifier.review_context(),
        },
        accepted_review,
        [&signatures[0], &signatures[1]],
        |actual| verifier.verify_native(actual),
    )?;
    if expected != *verifier.manifest() {
        return Err(Error::Package);
    }
    Ok(AcceptedMergePublication {
        receipt: receipt.clone(),
        manifest: expected,
    })
}
