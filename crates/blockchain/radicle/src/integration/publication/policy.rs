//! Exact accepted owner entries for the existing native publication predicate.

use super::{native_policy_context, review::AcceptedReviewPublication, CheckpointOwnerSnapshot};
use alloy_primitives::B256;
use outbe_primitives::radicle_publication::PublicationBody;
use radicle::{
    crypto::{BlsPublicKey, RepositoryKey, SubjectKind},
    git,
    identity::RepoId,
    storage::{
        publication::{EntryAuthorization, EntryRequest, Error, PublicationPolicy},
        refs::SIGREFS_BRANCH,
    },
};
use std::{collections::BTreeMap, sync::RwLock};

#[derive(Debug, PartialEq, Eq)]
struct Scope {
    chain: u64,
    genesis: B256,
    repository: RepoId,
    owner: RepositoryKey,
    descriptor: B256,
}

impl Scope {
    fn from_body(body: &PublicationBody) -> Result<Self, Error> {
        Ok(Self {
            chain: body.chain_id,
            genesis: body.genesis_hash,
            repository: git::Oid::from_sha1(body.repo_id.0).into(),
            owner: BlsPublicKey::from_bytes(body.owner_bls_public.0)
                .map_err(|_| Error::Configuration)?
                .into(),
            descriptor: body.descriptor_hash,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Grant {
    context: [u8; 32],
    receipt: [u8; 32],
}

#[derive(Debug, Default)]
struct AcceptedEntries {
    entries: BTreeMap<(git::Oid, u8), Grant>,
    receipts: BTreeMap<u64, B256>,
    finalized_blocks: BTreeMap<u64, B256>,
}

/// Archive authorization only: canonical ref promotion also needs the latest snapshot and CAS.
/// Rebuild this index from trusted receipts and verified packages; it never accepts peer grants.
#[derive(Debug)]
pub struct AcceptedRepositoryPolicy {
    scope: Scope,
    accepted: RwLock<AcceptedEntries>,
    recovery: RwLock<super::history::Recovery>,
}

impl AcceptedRepositoryPolicy {
    pub fn prepare_history(&self, history: super::history::AcceptedHistory) -> Result<bool, Error> {
        let mut recovery = self.recovery.write().map_err(|_| Error::Unavailable)?;
        let mut previous = alloy_primitives::B256::ZERO;
        for (index, receipt) in history.receipts.iter().enumerate() {
            let body = &receipt.receipt().body;
            let invalid_chain = self.scope != Scope::from_body(body)?
                || body.sequence != index as u64 + 1
                || body.previous_receipt_hash != previous;
            let invalid_receipt = receipt.receipt().receipt_hash != body.receipt_hash()
                || receipt.receipt().accepted_height > receipt.block().number;
            let invalid_bootstrap = index == 0
                && body.action
                    != outbe_primitives::radicle_publication::PublicationAction::Bootstrap;
            if invalid_chain || invalid_receipt || invalid_bootstrap {
                return Err(Error::Configuration);
            }
            previous = receipt.receipt().receipt_hash;
        }
        if history.receipts.is_empty()
            || history
                .descriptor
                .hash()
                .map_err(|_| Error::Configuration)?
                != self.scope.descriptor
        {
            return Err(Error::Configuration);
        }
        if let Some(old) = &recovery.plan {
            if old
                .receipts
                .iter()
                .zip(&history.receipts)
                .any(|(old, new)| old.receipt() != new.receipt())
            {
                return Err(Error::Configuration);
            }
            if history.receipts.len() <= old.receipts.len() {
                return Ok(false);
            }
        }
        recovery.plan = Some(history);
        Ok(true)
    }

    pub fn recovered_manifests(
        &self,
    ) -> Result<
        Vec<(
            crate::manager::FinalizedPublicationReceipt,
            super::PublicationManifest,
        )>,
        Error,
    > {
        let recovery = self.recovery.read().map_err(|_| Error::Unavailable)?;
        Ok(recovery
            .plan
            .iter()
            .flat_map(|plan| &plan.receipts)
            .filter_map(|receipt| {
                recovery
                    .manifests
                    .get(&receipt.receipt().receipt_hash)
                    .map(|manifest| (receipt.clone(), manifest.clone()))
            })
            .collect())
    }

    pub fn finish_history(&self) -> Result<(), Error> {
        *self.recovery.write().map_err(|_| Error::Unavailable)? =
            super::history::Recovery::default();
        Ok(())
    }
    /// Install the complete latest verified snapshot, including on a lagging local replica.
    pub fn install_snapshot(
        &self,
        repo: &radicle::storage::git::Repository,
        snapshot: &CheckpointOwnerSnapshot,
    ) -> Result<(), super::Error> {
        let checkpoint = snapshot.checkpoint().receipt();
        let accepted = self.accepted.read().map_err(|_| super::Error::Receipt)?;
        if self.scope != Scope::from_body(&checkpoint.body).map_err(|_| super::Error::Receipt)?
            || accepted.receipts.last_key_value()
                != Some((&checkpoint.body.sequence, &checkpoint.receipt_hash))
            || !accepted
                .entries
                .contains_key(&(snapshot.sigrefs(), SubjectKind::SignedRefs as u8))
        {
            return Err(super::Error::Receipt);
        }
        let identity = snapshot
            .owner_refs()
            .get(&radicle::storage::refs::IDENTITY_ROOT)
            .ok_or(super::Error::Snapshot)?;
        if !accepted
            .entries
            .contains_key(&(identity, SubjectKind::Identity as u8))
            || snapshot.owner_refs().iter().any(|(name, oid)| {
                name.as_str().starts_with("refs/cobs/")
                    && !accepted
                        .entries
                        .contains_key(&(*oid, SubjectKind::Cob as u8))
            })
        {
            return Err(super::Error::Receipt);
        }
        super::install_snapshot_refs(repo, snapshot)
    }

    /// Pin the derived network owner before bootstrap; no native entry is authorized yet.
    pub fn pending_bootstrap(
        job: &crate::integration::bootstrap::PreparedBootstrapDocument,
    ) -> Result<Self, Error> {
        Ok(Self {
            scope: Scope::from_body(job.body())?,
            accepted: RwLock::new(AcceptedEntries::default()),
            recovery: RwLock::default(),
        })
    }
    /// A fresh receiver pins the trusted owner while the accepted package is still unavailable.
    pub fn pending_receipt(
        receipt: &crate::manager::FinalizedPublicationReceipt,
    ) -> Result<Self, Error> {
        let root = receipt.receipt();
        let invalid_root = root.body.action
            != outbe_primitives::radicle_publication::PublicationAction::Bootstrap
            || root.body.sequence != 1
            || !root.body.previous_receipt_hash.is_zero();
        let invalid_receipt = root.receipt_hash != root.body.receipt_hash()
            || root.accepted_height > receipt.block().number;
        if invalid_root || invalid_receipt {
            return Err(Error::Configuration);
        }
        Ok(Self {
            scope: Scope::from_body(&root.body)?,
            accepted: RwLock::new(AcceptedEntries::default()),
            recovery: RwLock::default(),
        })
    }
    /// Pin the managed repository before any native owner entry is authorized.
    pub fn new(checkpoint: &CheckpointOwnerSnapshot) -> Result<Self, Error> {
        let receipt = checkpoint.checkpoint();
        let scope = Scope::from_body(&receipt.receipt().body)?;
        let mut accepted = AcceptedEntries::default();
        accepted.receipts.insert(
            receipt.receipt().body.sequence,
            receipt.receipt().receipt_hash,
        );
        accepted
            .finalized_blocks
            .insert(receipt.block().number, receipt.block().hash);
        Ok(Self {
            scope,
            accepted: RwLock::new(accepted),
            recovery: RwLock::default(),
        })
    }

    /// Only full native reconstruction plus the chain receipt can create these grants.
    pub fn accept_review(&self, proof: &AcceptedReviewPublication) -> Result<(), Error> {
        self.accept_patch(proof.receipt(), proof.manifest())
    }

    pub fn accept_merge(
        &self,
        proof: &super::merge::AcceptedMergePublication,
    ) -> Result<(), Error> {
        self.accept_patch(proof.receipt(), proof.manifest())
    }

    pub fn accept_material(
        &self,
        proof: &super::binding::AcceptedMaterialPublication,
    ) -> Result<(), Error> {
        if self.scope != Scope::from_body(&proof.receipt().receipt().body)? {
            return Err(Error::Configuration);
        }
        self.insert(proof.receipt(), vec![])
    }

    fn accept_patch(
        &self,
        receipt: &crate::manager::FinalizedPublicationReceipt,
        manifest: &super::PublicationManifest,
    ) -> Result<(), Error> {
        let body = &receipt.receipt().body;
        if self.scope != Scope::from_body(body)? {
            return Err(Error::Configuration);
        }
        let grants = manifest
            .refs()
            .iter()
            .filter(|update| {
                update.name == SIGREFS_BRANCH.as_str() || update.name.starts_with("refs/cobs/")
            })
            .map(|update| {
                let (kind, ordinal) = if update.name == SIGREFS_BRANCH.as_str() {
                    (SubjectKind::SignedRefs, 1)
                } else {
                    (SubjectKind::Cob, 0)
                };
                let oid = update.new.parse().map_err(|_| Error::Configuration)?;
                Ok((
                    (oid, kind as u8),
                    Grant {
                        context: native_policy_context(body, ordinal),
                        receipt: receipt.receipt().receipt_hash.0,
                    },
                ))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        self.insert(receipt, grants)
    }

    /// Root authority is granted only after complete native bootstrap reconstruction.
    pub fn accept_bootstrap(
        &self,
        proof: &crate::integration::bootstrap::AcceptedBootstrapPublication,
    ) -> Result<(), Error> {
        let receipt = proof.receipt();
        let body = &receipt.receipt().body;
        if self.scope != Scope::from_body(body)? {
            return Err(Error::Configuration);
        }
        let refs = proof
            .manifest()
            .owner_refs()
            .map_err(|_| Error::Configuration)?;
        let identity = refs
            .get(&radicle::storage::refs::IDENTITY_ROOT)
            .ok_or(Error::Configuration)?;
        let sigrefs = proof
            .manifest()
            .refs()
            .iter()
            .find(|update| update.name == SIGREFS_BRANCH.as_str())
            .ok_or(Error::Configuration)?
            .new
            .parse()
            .map_err(|_| Error::Configuration)?;
        let grants = [
            (identity, SubjectKind::Identity, 0),
            (identity, SubjectKind::Cob, 1),
            (sigrefs, SubjectKind::SignedRefs, 2),
        ]
        .into_iter()
        .map(|(oid, kind, ordinal)| {
            (
                (oid, kind as u8),
                Grant {
                    context: native_policy_context(body, ordinal),
                    receipt: receipt.receipt().receipt_hash.0,
                },
            )
        })
        .collect();
        self.insert(receipt, grants)
    }

    fn insert(
        &self,
        receipt: &crate::manager::FinalizedPublicationReceipt,
        grants: Vec<((git::Oid, u8), Grant)>,
    ) -> Result<(), Error> {
        let body = &receipt.receipt().body;
        let mut accepted = self.accepted.write().map_err(|_| Error::Unavailable)?;
        if accepted
            .receipts
            .get(&body.sequence)
            .is_some_and(|hash| *hash != receipt.receipt().receipt_hash)
            || accepted
                .finalized_blocks
                .get(&receipt.block().number)
                .is_some_and(|hash| *hash != receipt.block().hash)
            || grants
                .iter()
                .any(|(key, grant)| accepted.entries.get(key).is_some_and(|old| old != grant))
        {
            return Err(Error::Configuration);
        }
        accepted
            .receipts
            .insert(body.sequence, receipt.receipt().receipt_hash);
        accepted
            .finalized_blocks
            .insert(receipt.block().number, receipt.block().hash);
        accepted.entries.extend(grants);
        Ok(())
    }
}

impl PublicationPolicy for AcceptedRepositoryPolicy {
    fn needs_fetch_preparation(&self) -> bool {
        self.recovery
            .read()
            .map_or(true, |recovery| recovery.plan.is_some())
    }

    fn prepare_fetch(
        &self,
        repo: &radicle::storage::git::Repository,
        refs: &[(git::fmt::Qualified<'_>, git::Oid)],
    ) -> Result<(), Error> {
        let mut recovery = self.recovery.write().map_err(|_| Error::Unavailable)?;
        let super::history::Recovery { plan, manifests } = &mut *recovery;
        let plan = plan.as_ref().ok_or(Error::Unavailable)?;
        plan.recover(repo, refs, self, manifests).map_err(|error| {
            let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error.as_ref());
            while let Some(current) = cause {
                if current
                    .downcast_ref::<git::raw::Error>()
                    .is_some_and(|e| e.code() == git::raw::ErrorCode::NotFound)
                {
                    return Error::MissingObjects;
                }
                cause = current.source();
            }
            // Preserve the cause for node logs while keeping the native policy fail-closed.
            Error::Io(std::io::Error::other(error))
        })
    }
    fn id(&self) -> &str {
        "outbe.chain.publication.v1"
    }

    fn authorize_entry(&self, request: &EntryRequest) -> Result<EntryAuthorization, Error> {
        if request.repository != self.scope.repository || request.authority != self.scope.owner {
            return Err(Error::Denied);
        }
        let accepted = self.accepted.read().map_err(|_| Error::Unavailable)?;
        let grant = accepted
            .entries
            .get(&(request.entry, request.kind as u8))
            .ok_or(Error::Denied)?;
        Ok(EntryAuthorization::accepted(
            request.clone(),
            grant.context,
            grant.receipt,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{FinalizedBlock, FinalizedPublicationReceipt};
    use outbe_coderegistry::publication::PublicationReceipt;

    #[test]
    fn trusted_bootstrap_without_package_pins_owner_but_grants_no_entries() {
        let (body, _) = crate::integration::publication::tests::bootstrap_fixture();
        let mut receipt = FinalizedPublicationReceipt {
            block: FinalizedBlock {
                number: 10,
                hash: B256::repeat_byte(10),
            },
            receipt: PublicationReceipt {
                accepted_height: 9,
                receipt_hash: body.receipt_hash(),
                body,
            },
        };
        let policy = AcceptedRepositoryPolicy::pending_receipt(&receipt).unwrap();
        let owner = BlsPublicKey::from_bytes(receipt.receipt.body.owner_bls_public.0)
            .unwrap()
            .into();
        for kind in [
            SubjectKind::Identity,
            SubjectKind::Cob,
            SubjectKind::SignedRefs,
        ] {
            assert!(policy
                .authorize_entry(&EntryRequest {
                    repository: git::Oid::from_sha1(receipt.receipt.body.repo_id.0).into(),
                    authority: owner,
                    entry: git::Oid::from_sha1([5; 20]),
                    kind,
                })
                .is_err());
        }
        receipt.receipt.body.sequence = 2;
        receipt.receipt.receipt_hash = receipt.receipt.body.receipt_hash();
        assert!(AcceptedRepositoryPolicy::pending_receipt(&receipt).is_err());
    }
}
