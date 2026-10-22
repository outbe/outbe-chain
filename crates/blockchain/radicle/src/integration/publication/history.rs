//! Reconstruct accepted commitments from native Git objects and this node's finalized receipts.

use super::{
    native_policy_context, policy::AcceptedRepositoryPolicy, CheckpointOwnerSnapshot, OwnerRef,
    PublicationManifest, MAX_MANIFEST_BYTES,
};
use crate::{
    integration::{
        bootstrap, review::ReviewSigningContext, signing::accepted::AcceptedPublicationVerifier,
    },
    manager::{FinalizedPatchApproval, FinalizedPublicationReceipt},
};
use alloy_primitives::{B256, U256};
use outbe_primitives::{
    radicle_genesis::GovernmentsDescriptor, radicle_publication::PublicationAction,
};
use radicle::{
    cob::identity,
    crypto::{BlsEnvelope, RepositorySignature},
    git,
    storage::{
        git::Repository,
        publication::Error,
        refs::{Refs, IDENTITY_ROOT, REFS_BLOB_PATH, SIGNATURE_BLOB_PATH, SIGREFS_BRANCH},
        WriteRepository as _,
    },
};
use radicle_git_metadata::commit::bls::SignedCommit;
use std::collections::BTreeMap;

/// These values are read locally, never received from the Git peer.
#[derive(Debug)]
pub struct AcceptedHistory {
    pub descriptor: GovernmentsDescriptor,
    pub receipts: Vec<FinalizedPublicationReceipt>,
    pub approvals: BTreeMap<U256, FinalizedPatchApproval>,
}

#[derive(Debug, Default)]
pub(super) struct Recovery {
    pub plan: Option<AcceptedHistory>,
    pub manifests: BTreeMap<B256, PublicationManifest>,
}

struct NativeCheckpoint {
    sigrefs: git::Oid,
    signatures: Vec<BlsEnvelope>,
}

impl AcceptedHistory {
    pub(super) fn recover(
        &self,
        repo: &Repository,
        advertised: &[(git::fmt::Qualified<'_>, git::Oid)],
        policy: &AcceptedRepositoryPolicy,
        recovered: &mut BTreeMap<B256, PublicationManifest>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let root = self.receipts.first().ok_or(Error::Configuration)?;
        let owner =
            radicle::crypto::BlsPublicKey::from_bytes(root.receipt().body.owner_bls_public.0)?
                .into();
        let tip_name: git::fmt::Qualified = git::refs::storage::sigrefs(&owner).into();
        let tips: Vec<_> = advertised
            .iter()
            .filter(|(name, _)| *name == tip_name)
            .map(|(_, oid)| *oid)
            .collect();
        let [tip] = tips.as_slice() else {
            return Err(Error::Denied.into());
        };
        let checkpoints = self.native_checkpoints(repo, *tip)?;
        let mut snapshot = None;
        let mut reviews = BTreeMap::new();
        for receipt in &self.receipts {
            let body = &receipt.receipt().body;
            let native = checkpoints.get(&body.sequence);
            let signatures = match (body.action, native) {
                (PublicationAction::BindMaterial, _) => &[][..],
                (_, Some(native)) => native.signatures.as_slice(),
                // A peer may still be behind the local finalized chain.
                _ if snapshot.is_some() => break,
                _ => return Err(Error::Denied.into()),
            };
            let context = ReviewSigningContext {
                chain_id: body.chain_id,
                genesis_hash: body.genesis_hash.0,
                epoch: body.epoch,
                material_version: body.material_version,
                material_hash: body.material_hash.0,
                previous_receipt_hash: body.previous_receipt_hash.0,
            };
            let previous = || snapshot.as_ref().ok_or(Error::Denied);
            let approval = || self.approvals.get(&body.proposal_id).ok_or(Error::Denied);
            let manifest = match body.action {
                PublicationAction::Bootstrap => bootstrap::recover_bootstrap_manifest(
                    repo,
                    &self.descriptor,
                    body,
                    [&signatures[0], &signatures[1], &signatures[2]],
                )?,
                PublicationAction::Review => super::review::reconstruct_review(
                    repo,
                    approval()?,
                    previous()?,
                    &context,
                    [&signatures[0], &signatures[1]],
                    |_| Ok(()),
                )?,
                PublicationAction::Merge => super::merge::reconstruct(
                    repo,
                    approval()?,
                    previous()?,
                    reviews.get(&body.proposal_id).ok_or(Error::Denied)?,
                    &context,
                    [&signatures[0], &signatures[1]],
                    |_| Ok(()),
                )?,
                PublicationAction::BindMaterial => PublicationManifest::new(
                    vec![],
                    vec![],
                    vec![],
                    previous()?
                        .owner_refs()
                        .iter()
                        .map(|(name, oid)| OwnerRef {
                            name: name.to_string(),
                            oid: oid.to_string(),
                        })
                        .collect(),
                )?,
            };
            if let Some(native) = native {
                if !manifest.refs().iter().any(|r| {
                    r.name == SIGREFS_BRANCH.as_str() && r.new == native.sigrefs.to_string()
                }) {
                    return Err(Error::Denied.into());
                }
            }
            let verifier =
                AcceptedPublicationVerifier::from_receipt(receipt.clone(), manifest.clone())?;
            // Only the existing full accepted-proof paths can install entry grants.
            match body.action {
                PublicationAction::Bootstrap => {
                    let proof =
                        bootstrap::verify_accepted_bootstrap(repo, &self.descriptor, &verifier)?;
                    policy.accept_bootstrap(&proof)?;
                    snapshot = Some(CheckpointOwnerSnapshot::bootstrap(
                        receipt.clone(),
                        &manifest,
                    )?);
                }
                PublicationAction::Review => {
                    let proof = super::review::verify_accepted_review_publication(
                        repo,
                        approval()?,
                        previous()?,
                        &verifier,
                    )?;
                    policy.accept_review(&proof)?;
                    reviews.insert(body.proposal_id, proof);
                    snapshot
                        .as_mut()
                        .ok_or(Error::Denied)?
                        .advance(receipt.clone(), &manifest)?;
                }
                PublicationAction::Merge => {
                    let proof = super::merge::verify_accepted_merge_publication(
                        repo,
                        approval()?,
                        previous()?,
                        reviews.get(&body.proposal_id).ok_or(Error::Denied)?,
                        &verifier,
                    )?;
                    policy.accept_merge(&proof)?;
                    snapshot
                        .as_mut()
                        .ok_or(Error::Denied)?
                        .advance(receipt.clone(), &manifest)?;
                }
                PublicationAction::BindMaterial => {
                    policy.accept_material(
                        &super::binding::verify_accepted_material_publication(
                            previous()?,
                            &verifier,
                        )?,
                    )?;
                    snapshot
                        .as_mut()
                        .ok_or(Error::Denied)?
                        .advance(receipt.clone(), &manifest)?;
                }
            }
            recovered.insert(receipt.receipt().receipt_hash, manifest);
        }
        Ok(())
    }

    fn native_checkpoints(
        &self,
        repo: &Repository,
        mut tip: git::Oid,
    ) -> Result<BTreeMap<u64, NativeCheckpoint>, Box<dyn std::error::Error + Send + Sync>> {
        let expected: BTreeMap<_, _> = self
            .receipts
            .iter()
            .filter(|r| r.receipt().body.action != PublicationAction::BindMaterial)
            .map(|r| {
                let body = &r.receipt().body;
                (
                    native_policy_context(
                        body,
                        if body.action == PublicationAction::Bootstrap {
                            2
                        } else {
                            1
                        },
                    ),
                    body,
                )
            })
            .collect();
        let mut checkpoints = BTreeMap::new();
        // The local immutable receipt count bounds the native transparency-log traversal.
        for _ in 0..self.receipts.len() {
            object(
                repo,
                tip,
                git::raw::ObjectType::Commit,
                radicle::crypto::repository::MAX_NATIVE_PAYLOAD,
            )?;
            let commit = repo.raw().find_commit(tip.into())?;
            if commit.parent_count() > 1 {
                return Err(Error::Denied.into());
            }
            let tree = commit.tree_id().into();
            let signature = blob(repo, tree, SIGNATURE_BLOB_PATH, 269)?;
            let signature = BlsEnvelope::from_bytes(&signature)?;
            let body = expected.get(&signature.context()).ok_or(Error::Denied)?;
            let refs = blob(repo, tree, REFS_BLOB_PATH, MAX_MANIFEST_BYTES)?;
            let canonical = Refs::from_canonical(&refs)?;
            if canonical.canonical() != refs {
                return Err(Error::Denied.into());
            }
            let parent = if commit.parent_count() == 0 {
                None
            } else {
                Some(commit.parent_id(0)?)
            };
            RepositorySignature::from(signature.clone()).verify_signed_refs(
                radicle::crypto::BlsPublicKey::from_bytes(body.owner_bls_public.0)?.into(),
                body.repo_id.0,
                signature.context(),
                parent
                    .map(|p| p.as_bytes().try_into())
                    .transpose()?
                    .unwrap_or([0; 20]),
                &refs,
            )?;
            let signatures = native_signatures(repo, body, &canonical, signature)?;
            if checkpoints
                .insert(
                    body.sequence,
                    NativeCheckpoint {
                        sigrefs: tip,
                        signatures,
                    },
                )
                .is_some()
            {
                return Err(Error::Denied.into());
            }
            match parent {
                Some(parent) => tip = parent.into(),
                None => return Ok(checkpoints),
            }
        }
        Err(Error::Denied.into())
    }
}

fn native_signatures(
    repo: &Repository,
    body: &outbe_primitives::radicle_publication::PublicationBody,
    canonical: &Refs,
    signature: BlsEnvelope,
) -> Result<Vec<BlsEnvelope>, Box<dyn std::error::Error + Send + Sync>> {
    let name = if body.action == PublicationAction::Bootstrap {
        IDENTITY_ROOT.clone()
    } else {
        git::fmt::Qualified::from_refstr(&git::fmt::RefString::try_from(format!(
            "refs/cobs/xyz.radicle.patch/{}",
            git::Oid::from_sha1(body.patch_id.0)
        ))?)
        .ok_or(Error::Denied)?
        .to_owned()
    };
    let entry = canonical.get(&name).ok_or(Error::Denied)?;
    let bytes = object(
        repo,
        entry,
        git::raw::ObjectType::Commit,
        radicle::crypto::repository::MAX_NATIVE_PAYLOAD,
    )?;
    let cob = SignedCommit::parse(&bytes)?;
    let cob_signature = BlsEnvelope::from_hex(cob.envelope())?;
    let mut signatures = vec![cob_signature, signature];
    if body.action == PublicationAction::Bootstrap {
        let action: identity::Action = serde_json::from_slice(&blob(
            repo,
            git::Oid::from_sha1(cob.unsigned().tree()),
            "0",
            MAX_MANIFEST_BYTES,
        )?)?;
        let identity::Action::Revision {
            signature: RepositorySignature::Bls12381G2(document),
            ..
        } = action
        else {
            return Err(Error::Denied.into());
        };
        signatures.insert(0, *document);
    }
    Ok(signatures)
}

fn object(
    repo: &Repository,
    oid: git::Oid,
    kind: git::raw::ObjectType,
    maximum: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let odb = repo.raw().odb()?;
    let (size, actual) = odb.read_header(oid.into())?;
    if size > maximum || actual != kind {
        return Err(Error::Denied.into());
    }
    let bytes = odb.read(oid.into())?.data().to_vec();
    Ok(bytes)
}

fn blob(
    repo: &Repository,
    tree: git::Oid,
    name: &str,
    maximum: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    object(
        repo,
        tree,
        git::raw::ObjectType::Tree,
        radicle::crypto::repository::MAX_NATIVE_PAYLOAD,
    )?;
    let tree = repo.raw().find_tree(tree.into())?;
    let entry = tree.get_name(name).ok_or(Error::Denied)?;
    object(repo, entry.id().into(), git::raw::ObjectType::Blob, maximum)
}
