//! Deterministic native Merge after the exact accepted Review; preparation never changes refs.
use super::{
    publication::{review::AcceptedReviewPublication, CheckpointOwnerSnapshot},
    review::{self, PreparedPatchReview, PreparedReviewRefs, ReviewSigningContext},
};
use crate::manager::{FinalizedPatchApproval, FinalizedPublicationReceipt};
use outbe_coderegistry::{payload::PatchVotePayloadV1, PatchApprovalRecord};
use radicle::{
    cob::{self, object, patch},
    crypto::{BlsEnvelope, RepositoryKey},
    git,
    storage::{git::Repository, WriteRepository as _},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Merge requires the exact accepted Review and latest checkpoint")]
    Context,
    #[error("approved target has moved; a new Vote is required")]
    TargetMoved,
    #[error("approved revision has a merge conflict")]
    Conflict,
    #[error(transparent)]
    Preparation(#[from] review::Error),
    #[error(transparent)]
    Graph(#[from] cob::error::Retrieve),
    #[error(transparent)]
    Git(#[from] git::raw::Error),
}

pub struct CheckpointPreparedMerge {
    inner: PreparedPatchReview,
    snapshot: CheckpointOwnerSnapshot,
    approval: PatchApprovalRecord,
    review: FinalizedPublicationReceipt,
    result: git::Oid,
}
impl std::ops::Deref for CheckpointPreparedMerge {
    type Target = PreparedPatchReview;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl CheckpointPreparedMerge {
    pub(crate) fn approval(&self) -> &PatchApprovalRecord {
        &self.approval
    }
    pub(crate) fn snapshot(&self) -> &CheckpointOwnerSnapshot {
        &self.snapshot
    }
    pub(crate) fn review(&self) -> &FinalizedPublicationReceipt {
        &self.review
    }
    pub fn result(&self) -> git::Oid {
        self.result
    }
    pub fn attach(
        self,
        repo: &Repository,
        signature: &BlsEnvelope,
    ) -> Result<(cob::Entry, CheckpointPreparedMergeRefs), Error> {
        let mut refs = self.snapshot.owner_refs().clone();
        refs.insert(
            git::fmt::qualified!("refs/heads/main").to_ref_string(),
            self.result,
        );
        let (entry, inner) = self
            .inner
            .attach(repo, signature, refs, self.snapshot.sigrefs())?;
        Ok((
            entry,
            CheckpointPreparedMergeRefs {
                inner,
                snapshot: self.snapshot,
                approval: self.approval,
                review: self.review,
                result: self.result,
            },
        ))
    }
}

pub struct CheckpointPreparedMergeRefs {
    inner: PreparedReviewRefs,
    snapshot: CheckpointOwnerSnapshot,
    approval: PatchApprovalRecord,
    review: FinalizedPublicationReceipt,
    result: git::Oid,
}
impl std::ops::Deref for CheckpointPreparedMergeRefs {
    type Target = PreparedReviewRefs;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl CheckpointPreparedMergeRefs {
    pub(crate) fn approval(&self) -> &PatchApprovalRecord {
        &self.approval
    }
    pub(crate) fn snapshot(&self) -> &CheckpointOwnerSnapshot {
        &self.snapshot
    }
    pub(crate) fn review(&self) -> &FinalizedPublicationReceipt {
        &self.review
    }
    pub fn result(&self) -> git::Oid {
        self.result
    }
    pub fn attach(
        self,
        repo: &git::raw::Repository,
        signature: BlsEnvelope,
    ) -> Result<radicle::storage::refs::sigrefs::VerifiedCommit, Error> {
        self.inner.attach(repo, signature).map_err(Into::into)
    }
}

pub fn prepare_patch_merge_from_checkpoint(
    repo: &Repository,
    approval: &FinalizedPatchApproval,
    snapshot: &CheckpointOwnerSnapshot,
    accepted_review: &AcceptedReviewPublication,
    context: &ReviewSigningContext,
) -> Result<CheckpointPreparedMerge, Error> {
    let decision = PatchVotePayloadV1::decode(approval.approval.payload.as_bytes())
        .map_err(|_| Error::Context)?;
    let checkpoint = snapshot.checkpoint().receipt();
    let reviewed = accepted_review.receipt().receipt();
    if reviewed.body.chain_id != checkpoint.body.chain_id
        || reviewed.body.genesis_hash != checkpoint.body.genesis_hash
        || reviewed.body.owner_bls_public != checkpoint.body.owner_bls_public
        || reviewed.body.descriptor_hash != checkpoint.body.descriptor_hash
        || context.chain_id != checkpoint.body.chain_id
        || context.genesis_hash != checkpoint.body.genesis_hash.0
        || reviewed.body.proposal_id != approval.approval.proposal_id
        || reviewed.body.repo_id != decision.repo_id
        || reviewed.body.patch_id != decision.patch_id
        || reviewed.body.revision_id != decision.revision_id
        || reviewed.body.expected_target_head != decision.expected_target_head
        || reviewed.body.sequence > checkpoint.body.sequence
        || checkpoint.body.repo_id != decision.repo_id
        || context.previous_receipt_hash != checkpoint.receipt_hash.0
        || context.epoch != checkpoint.body.epoch
        || context.material_version != checkpoint.body.material_version
        || context.material_hash != checkpoint.body.material_hash.0
    {
        return Err(Error::Context);
    }
    let target = snapshot
        .owner_refs()
        .head(git::fmt::refname!("main"))
        .ok_or(Error::Context)?;
    if target.into_sha1() != Some(decision.expected_target_head.0) {
        return Err(Error::TargetMoved);
    }
    let owner = RepositoryKey::from(
        radicle::crypto::BlsPublicKey::from_bytes(checkpoint.body.owner_bls_public.0)
            .map_err(|_| Error::Context)?,
    );
    let patch: patch::PatchId = git::Oid::from_sha1(decision.patch_id.0).into();
    let revision: patch::RevisionId = git::Oid::from_sha1(decision.revision_id.0).into();
    let name = git::refs::storage::cob(&owner, &patch::TYPENAME, &patch);
    let plain = name.strip_namespace_recursive();
    let review: git::Oid = accepted_review
        .manifest()
        .refs()
        .iter()
        .find(|r| r.name == plain.as_str())
        .ok_or(Error::Context)?
        .new
        .parse()
        .map_err(|_| Error::Context)?;
    let mut tips = vec![review];
    tips.extend(snapshot.owner_refs().get(&plain));
    tips.sort_unstable();
    tips.dedup();
    let pinned = object::Objects::from(
        tips.iter()
            .map(|tip| object::Reference {
                name: name.to_ref_string(),
                target: object::Commit { id: *tip },
            })
            .collect::<Vec<_>>(),
    );
    let object = cob::get_at::<patch::Patch, _>(repo, &patch::TYPENAME, &patch, &pinned)?
        .ok_or(Error::Context)?;
    if tips.iter().any(|tip| !object.history.graph().contains(tip)) {
        return Err(Error::Context);
    }
    let state = object.object.revision(&revision).ok_or(Error::Context)?;
    if state.review_by(&owner).and_then(|r| r.verdict()) != Some(patch::Verdict::Accept) {
        return Err(Error::Context);
    }
    let head = state.head();
    let result = if head == target || repo.raw().graph_descendant_of(head.into(), target.into())? {
        head
    } else {
        let base = repo.raw().find_commit((*state.base()).into())?.tree()?;
        let ours = repo.raw().find_commit(target.into())?;
        let theirs = repo.raw().find_commit(head.into())?;
        let mut index = repo
            .raw()
            .merge_trees(&base, &ours.tree()?, &theirs.tree()?, None)?;
        if index.has_conflicts() {
            return Err(Error::Conflict);
        }
        let tree = repo.raw().find_tree(index.write_tree_to(repo.raw())?)?;
        let seconds =
            i64::try_from(approval.approval.approved_timestamp).map_err(|_| Error::Context)?;
        let metadata = git::raw::Signature::new(
            "Outbe Network",
            "network@outbe.invalid",
            &git::raw::Time::new(seconds, 0),
        )?;
        repo.raw()
            .commit(
                None,
                &metadata,
                &metadata,
                "Merge patch\n",
                &tree,
                &[&ours, &theirs],
            )?
            .into()
    };
    let identity = snapshot
        .owner_refs()
        .get(&radicle::storage::refs::IDENTITY_ROOT)
        .ok_or(Error::Context)?;
    let inner = review::prepare_patch_merge(
        repo,
        approval,
        identity,
        snapshot.owner_refs().get(&plain),
        context,
        review,
        result,
    )?;
    Ok(CheckpointPreparedMerge {
        inner,
        snapshot: snapshot.clone(),
        approval: approval.approval.clone(),
        review: accepted_review.receipt().clone(),
        result,
    })
}
