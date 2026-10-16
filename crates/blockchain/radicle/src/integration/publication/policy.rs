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
}

impl AcceptedRepositoryPolicy {
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
        })
    }
    /// A fresh receiver pins the trusted owner while the accepted package is still unavailable.
    pub fn pending_receipt(
        receipt: &crate::manager::FinalizedPublicationReceipt,
    ) -> Result<Self, Error> {
        let root = receipt.receipt();
        if root.body.action != outbe_primitives::radicle_publication::PublicationAction::Bootstrap
            || root.body.sequence != 1
            || !root.body.previous_receipt_hash.is_zero()
            || root.receipt_hash != root.body.receipt_hash()
            || root.accepted_height > receipt.block().number
        {
            return Err(Error::Configuration);
        }
        Ok(Self {
            scope: Scope::from_body(&root.body)?,
            accepted: RwLock::new(AcceptedEntries::default()),
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
    use outbe_radicleregistry::publication::PublicationReceipt;

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
