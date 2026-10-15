//! Native Review preparation for an exact finalized Vote decision.

use commonware_cryptography::{Hasher as _, Sha256};
use outbe_radicleregistry::payload::PatchVotePayloadV1;
use radicle::storage::refs::{
    sigrefs::{git::Committer, write::PreparedSignedRefs},
    Refs, SIGREFS_PARENT,
};
use radicle::{
    cob::{self, object, patch, store::Transaction},
    crypto::{BlsEnvelope, RepositoryKey, RepositorySubject},
    git,
    storage::{git::Repository, ReadRepository, WriteRepository},
};
use radicle_git_metadata::{
    author::{Author, Time},
    commit::bls::UnsignedCommit,
};

use super::publication::CheckpointOwnerSnapshot;
use crate::manager::FinalizedPatchApproval;

#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid finalized patch approval")]
    Approval,
    #[error("repository must have the sole network BLS owner and default branch main")]
    Authority,
    #[error("approved revision is absent from the pinned patch graph")]
    Revision,
    #[error("patch does not target main")]
    Target,
    #[error(transparent)]
    Identity(#[from] radicle::identity::doc::DocError),
    #[error(transparent)]
    Graph(#[from] cob::error::Retrieve),
    #[error(transparent)]
    Git(#[from] git::raw::Error),
    #[error(transparent)]
    Transaction(#[from] cob::store::Error),
    #[error(transparent)]
    Signature(#[from] radicle::crypto::repository::Error),
    #[error(transparent)]
    Attach(#[from] cob::GitLoadError),
    #[error(transparent)]
    SignedRefs(#[from] radicle::storage::refs::sigrefs::write::error::Native),
    #[error("owner refs differ from the accepted preparation snapshot")]
    Snapshot,
}

pub struct PreparedPatchReview {
    patch: patch::PatchId,
    revision: patch::RevisionId,
    owner: RepositoryKey,
    commit: UnsignedCommit,
    subject: RepositorySubject,
    signed_refs_context: [u8; 32],
    context: [u8; 32],
    metadata: Author,
    previous_owner_tip: Option<git::Oid>,
    signing: ReviewSigningContext,
}

/// Review with its complete preparation snapshot pinned to an accepted checkpoint.
pub struct CheckpointPreparedReview {
    review: PreparedPatchReview,
    snapshot: CheckpointOwnerSnapshot,
    approval: outbe_radicleregistry::PatchApprovalRecord,
}

impl std::ops::Deref for CheckpointPreparedReview {
    type Target = PreparedPatchReview;
    fn deref(&self) -> &Self::Target {
        &self.review
    }
}

impl CheckpointPreparedReview {
    pub(crate) fn approval(&self) -> &outbe_radicleregistry::PatchApprovalRecord {
        &self.approval
    }
    pub(crate) fn snapshot(&self) -> &CheckpointOwnerSnapshot {
        &self.snapshot
    }
    pub fn attach(
        self,
        repo: &Repository,
        signature: &BlsEnvelope,
    ) -> Result<(cob::Entry, CheckpointPreparedReviewRefs), Error> {
        let (entry, refs) = self.review.attach(
            repo,
            signature,
            self.snapshot.owner_refs().clone(),
            self.snapshot.sigrefs(),
        )?;
        Ok((
            entry,
            CheckpointPreparedReviewRefs {
                refs,
                snapshot: self.snapshot,
                approval: self.approval,
            },
        ))
    }
}

pub struct CheckpointPreparedReviewRefs {
    refs: PreparedReviewRefs,
    snapshot: CheckpointOwnerSnapshot,
    approval: outbe_radicleregistry::PatchApprovalRecord,
}

impl std::ops::Deref for CheckpointPreparedReviewRefs {
    type Target = PreparedReviewRefs;
    fn deref(&self) -> &Self::Target {
        &self.refs
    }
}

impl CheckpointPreparedReviewRefs {
    pub(crate) fn approval(&self) -> &outbe_radicleregistry::PatchApprovalRecord {
        &self.approval
    }
    pub(crate) fn snapshot(&self) -> &CheckpointOwnerSnapshot {
        &self.snapshot
    }
    pub fn attach(
        self,
        repo: &git::raw::Repository,
        signature: BlsEnvelope,
    ) -> Result<radicle::storage::refs::sigrefs::VerifiedCommit, Error> {
        self.refs.attach(repo, signature)
    }
}

/// The actor separately rechecks current finalized permission/material before signing/publishing.
pub fn prepare_patch_review_from_checkpoint(
    repo: &Repository,
    approval: &FinalizedPatchApproval,
    snapshot: &CheckpointOwnerSnapshot,
    context: &ReviewSigningContext,
) -> Result<CheckpointPreparedReview, Error> {
    let receipt = snapshot.checkpoint().receipt();
    let body = &receipt.body;
    if context.chain_id != body.chain_id
        || context.genesis_hash != body.genesis_hash.0
        || context.epoch != body.epoch
        || context.material_version != body.material_version
        || context.material_hash != body.material_hash.0
        || context.previous_receipt_hash != receipt.receipt_hash.0
        || repo.id().into_sha1() != Some(body.repo_id.0)
    {
        return Err(Error::Snapshot);
    }
    let identity = snapshot
        .owner_refs()
        .get(&radicle::storage::refs::IDENTITY_ROOT)
        .ok_or(Error::Snapshot)?;
    let owner: RepositoryKey =
        radicle::crypto::BlsPublicKey::from_bytes(body.owner_bls_public.0)?.into();
    let decision = PatchVotePayloadV1::decode(approval.approval.payload.as_bytes())
        .map_err(|_| Error::Approval)?;
    let patch: patch::PatchId = git::Oid::from_sha1(decision.patch_id.0).into();
    let name =
        git::refs::storage::cob(&owner, &patch::TYPENAME, &patch).strip_namespace_recursive();
    let previous = snapshot.owner_refs().get(&name);
    let review = prepare_patch_review(repo, approval, identity, previous, context)?;
    if review.owner() != owner {
        return Err(Error::Authority);
    }
    Ok(CheckpointPreparedReview {
        review,
        snapshot: snapshot.clone(),
        approval: approval.approval.clone(),
    })
}

impl PreparedPatchReview {
    pub fn revision(&self) -> patch::RevisionId {
        self.revision
    }
    pub fn owner(&self) -> RepositoryKey {
        self.owner
    }
    pub fn subject(&self) -> &RepositorySubject {
        &self.subject
    }
    pub(crate) fn signing_context(&self) -> &ReviewSigningContext {
        &self.signing
    }
    /// Store the signed Review and prepare its exact owner refs. No refs are promoted here.
    pub fn attach(
        self,
        repo: &Repository,
        signature: &BlsEnvelope,
        mut accepted_refs: Refs,
        previous_sigrefs: git::Oid,
    ) -> Result<(cob::Entry, PreparedReviewRefs), Error> {
        let name = git::refs::storage::cob(&self.owner, &patch::TYPENAME, &self.patch)
            .strip_namespace_recursive();
        if accepted_refs.get(&name) != self.previous_owner_tip {
            return Err(Error::Snapshot);
        }
        signature.verify(&self.subject, self.owner)?;
        let entry = cob::attach_native(
            repo.raw(),
            self.commit,
            repo.id().into_sha1().ok_or(Error::Approval)?,
            self.context,
            signature,
        )?;
        accepted_refs.insert(name.to_ref_string(), entry.id);
        accepted_refs.insert(SIGREFS_PARENT.to_ref_string(), git::Oid::ZERO_SHA1);
        let refs = PreparedSignedRefs::new(
            accepted_refs,
            repo.id(),
            self.owner,
            self.signed_refs_context,
            Some(previous_sigrefs),
            Committer::new(self.metadata),
            "Update signed refs\n".into(),
        )?;
        let subject = refs.subject()?;
        Ok((
            entry,
            PreparedReviewRefs {
                refs,
                subject,
                owner: self.owner,
                signing: self.signing,
            },
        ))
    }
}

/// Exact owner refs derived from an attached, authorized native Review.
pub struct PreparedReviewRefs {
    refs: PreparedSignedRefs,
    subject: RepositorySubject,
    owner: RepositoryKey,
    signing: ReviewSigningContext,
}

impl PreparedReviewRefs {
    pub fn subject(&self) -> &RepositorySubject {
        &self.subject
    }
    pub(crate) fn owner(&self) -> RepositoryKey {
        self.owner
    }
    pub(crate) fn signing_context(&self) -> &ReviewSigningContext {
        &self.signing
    }
    /// Store verified refs objects without promoting any reference.
    pub fn attach(
        self,
        repo: &git::raw::Repository,
        signature: BlsEnvelope,
    ) -> Result<radicle::storage::refs::sigrefs::VerifiedCommit, Error> {
        Ok(self.refs.attach(repo, signature)?)
    }
}

/// Authenticated network/material inputs from the current finalized signing view.
#[derive(Clone, Debug)]
pub struct ReviewSigningContext {
    pub chain_id: u64,
    pub genesis_hash: [u8; 32],
    pub epoch: u64,
    pub material_version: u64,
    pub material_hash: [u8; 32],
    pub previous_receipt_hash: [u8; 32],
}

impl ReviewSigningContext {
    fn policy_context(
        &self,
        decision: &PatchVotePayloadV1,
        approval: &FinalizedPatchApproval,
        ordinal: u8,
    ) -> [u8; 32] {
        Sha256::hash(&[
            b"OPJ1",
            &self.chain_id.to_be_bytes(),
            &self.genesis_hash,
            decision.repo_id.as_slice(),
            &approval.approval.proposal_id.to_be_bytes::<32>(),
            decision.patch_id.as_slice(),
            decision.revision_id.as_slice(),
            decision.expected_target_head.as_slice(),
            &[2],
            &[ordinal],
            &self.epoch.to_be_bytes(),
            &self.material_version.to_be_bytes(),
            &self.material_hash,
            &self.previous_receipt_hash,
        ])
        .0
    }
}

/// `identity` and `previous_owner_tip` come from the previous accepted network snapshot.
/// The approval must come from the exact finalized-state reader, never a peer request.
pub fn prepare_patch_review(
    repo: &Repository,
    approval: &FinalizedPatchApproval,
    identity: git::Oid,
    previous_owner_tip: Option<git::Oid>,
    context: &ReviewSigningContext,
) -> Result<PreparedPatchReview, Error> {
    let decision = PatchVotePayloadV1::decode(approval.approval.payload.as_bytes())
        .map_err(|_| Error::Approval)?;
    if approval.approval.approved_height == 0
        || approval.approval.approved_height > approval.block.number
        || repo.id().into_sha1() != Some(decision.repo_id.0)
    {
        return Err(Error::Approval);
    }
    let timestamp =
        i64::try_from(approval.approval.approved_timestamp).map_err(|_| Error::Approval)?;
    let doc = repo.identity_doc_at(identity)?;
    let owner = *doc.delegates().first().as_key();
    if doc.delegates().len() != 1
        || owner.as_ed25519().is_some()
        || doc.threshold() != 1
        || doc.default_branch().map_err(|_| Error::Authority)?.as_str() != "refs/heads/main"
    {
        return Err(Error::Authority);
    }
    let patch: patch::PatchId = git::Oid::from_sha1(decision.patch_id.0).into();
    let revision: patch::RevisionId = git::Oid::from_sha1(decision.revision_id.0).into();
    let mut tips = vec![*revision];
    tips.extend(previous_owner_tip);
    tips.sort_unstable();
    tips.dedup();
    let name = git::refs::storage::cob(&owner, &patch::TYPENAME, &patch);
    let pinned = object::Objects::from(
        tips.iter()
            .map(|id| object::Reference {
                name: name.to_ref_string(),
                target: object::Commit { id: *id },
            })
            .collect::<Vec<_>>(),
    );
    let object = cob::get_at::<patch::Patch, _>(repo, &patch::TYPENAME, &patch, &pinned)?
        .ok_or(Error::Revision)?;
    // The evaluator can prune invalid branches; every pinned tip must survive evaluation.
    if tips.iter().any(|tip| !object.history.graph().contains(tip))
        || object.object.revision(&revision).is_none()
    {
        return Err(Error::Revision);
    }
    match object.object.target() {
        patch::MergeTarget::Delegates => (),
        patch::MergeTarget::Branch(branch) if branch.as_str() == "refs/heads/main" => (),
        _ => return Err(Error::Target),
    }
    let revision_state = object.object.revision(&revision).ok_or(Error::Revision)?;
    repo.raw().find_commit((*revision_state.base()).into())?;
    repo.raw().find_commit(revision_state.head().into())?;
    repo.raw()
        .find_commit(git::Oid::from_sha1(decision.expected_target_head.0).into())?;
    let mut transaction = Transaction::<patch::Patch, Repository>::default();
    transaction.review(revision, Some(patch::Verdict::Accept), None, vec![])?;
    let metadata = Author {
        name: "Outbe Network".into(),
        email: "network@outbe.invalid".into(),
        time: Time::new(timestamp, 0),
    };
    let commit = transaction.prepare_native(
        repo,
        Some(identity),
        tips,
        "Accept patch\n".into(),
        metadata.clone(),
    )?;
    Ok(PreparedPatchReview {
        patch,
        revision,
        owner,
        subject: RepositorySubject::cob(
            decision.repo_id.0,
            context.policy_context(&decision, approval, 0),
            commit.tree(),
            commit.as_bytes(),
        ),
        signed_refs_context: context.policy_context(&decision, approval, 1),
        context: context.policy_context(&decision, approval, 0),
        metadata,
        previous_owner_tip,
        signing: context.clone(),
        commit,
    })
}
