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
    /// Pin the derived network owner before bootstrap; no native entry is authorized yet.
    pub fn pending_bootstrap(
        job: &crate::integration::bootstrap::PreparedBootstrapDocument,
    ) -> Result<Self, Error> {
        Ok(Self {
            scope: Scope::from_body(job.body())?,
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
        let receipt = proof.receipt();
        let body = &receipt.receipt().body;
        if self.scope != Scope::from_body(body)? {
            return Err(Error::Configuration);
        }
        let grants = proof
            .manifest()
            .refs()
            .iter()
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
