//! The existing native Merge action and its complete signed refs form one publication.
use super::{
    review::{patch_body, AcceptedReviewPublication, Error},
    CheckpointOwnerSnapshot, NativeSubject, ObjectCommitment, ObjectKind, OwnerRef,
    PublicationManifest, RefUpdate,
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
use outbe_primitives::radicle_publication::{PublicationAction, PublicationBody};
use outbe_radicleregistry::{payload::PatchVotePayloadV1, PatchApprovalRecord};
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

pub fn prepare_merge_publication(
    repo: &Repository,
    state: &FinalizedPatchSigningState,
    snapshot: &CheckpointOwnerSnapshot,
    accepted_review: &AcceptedReviewPublication,
    material: &RepositorySigningMaterial,
    signatures: [&BlsEnvelope; 2],
    witness: &RepositoryWitnessBundle,
) -> Result<VerifiedMergePublication, Error> {
    let mut body = patch_body(
        state,
        snapshot.checkpoint(),
        &state.approval().approval,
        material,
        PublicationAction::Merge,
    )?;
    let manifest = reconstruct(
        repo,
        state.approval(),
        snapshot,
        accepted_review,
        &material.review_context(body.previous_receipt_hash.0),
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

pub fn verify_merge_publication(
    repo: &Repository,
    state: &FinalizedPatchSigningState,
    snapshot: &CheckpointOwnerSnapshot,
    accepted_review: &AcceptedReviewPublication,
    material: &RepositorySigningMaterial,
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
    let expected = prepare_merge_publication(
        repo,
        state,
        snapshot,
        accepted_review,
        material,
        [&signatures[0], &signatures[1]],
        witness,
    )?;
    if expected.body != *body || expected.manifest != *manifest {
        return Err(Error::Package);
    }
    Ok(expected)
}

fn reconstruct(
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
    let new_objects = object_closure(repo.raw(), vec![entry.id, sigrefs, result], false)?;
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
    witness: &RepositoryWitnessBundle,
) -> Result<AcceptedMergePublication, Error> {
    let receipt = verifier.receipt();
    let body = &receipt.receipt().body;
    let before = previous.checkpoint().receipt();
    let decision = PatchVotePayloadV1::decode(approval.approval.payload.as_bytes())
        .map_err(|_| Error::Context)?;
    if body.action != PublicationAction::Merge
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
    let expected = reconstruct(
        repo,
        approval,
        previous,
        accepted_review,
        &verifier.review_context(),
        [&signatures[0], &signatures[1]],
        |actual| witness.verify_accepted(verifier, actual),
    )?;
    if expected != *verifier.manifest() {
        return Err(Error::Package);
    }
    Ok(AcceptedMergePublication {
        receipt: receipt.clone(),
        manifest: expected,
    })
}
