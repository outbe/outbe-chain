//! Deterministic native genesis objects. Preparation never promotes repository refs.

use super::{
    publication::{
        native_policy_context, NativeSubject, ObjectCommitment, ObjectKind, OwnerRef,
        PublicationManifest, RefUpdate,
    },
    review::object_closure,
    signing::RepositorySigningMaterial,
};
use alloy_primitives::{B256, U256};
use commonware_cryptography::{Hasher as _, Sha256};
use outbe_primitives::{
    radicle_genesis::GovernmentsDescriptor,
    radicle_publication::{PublicationAction, PublicationBody},
};
use radicle::{
    cob::{self, identity, store::Transaction},
    crypto::{BlsEnvelope, BlsPublicKey, RepositoryKey, RepositorySubject},
    git,
    identity::{
        doc::{Doc, Visibility},
        Project, RepoId,
    },
    storage::{
        git::Repository,
        refs::{
            sigrefs::{git::Committer, write::PreparedSignedRefs},
            Refs, IDENTITY_ROOT, SIGREFS_BRANCH,
        },
        ReadRepository as _, WriteRepository as _,
    },
};
use radicle_git_metadata::{author::Author, commit::bls::UnsignedCommit};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("bootstrap descriptor, repository or initial content does not match genesis")]
    Descriptor,
    #[error(transparent)]
    Git(#[from] git::raw::Error),
    #[error(transparent)]
    Repository(#[from] radicle::storage::RepositoryError),
    #[error("bootstrap owner reference conflicts with the accepted package")]
    RefConflict,
    #[error(transparent)]
    Doc(#[from] radicle::identity::doc::DocError),
    #[error(transparent)]
    Native(#[from] radicle::crypto::repository::Error),
    #[error(transparent)]
    Transaction(#[from] cob::store::Error),
    #[error(transparent)]
    Attach(#[from] cob::GitLoadError),
    #[error(transparent)]
    SignedRefs(#[from] radicle::storage::refs::sigrefs::write::error::Native),
    #[error(transparent)]
    Manifest(#[from] super::publication::Error),
    #[error(transparent)]
    Signing(#[from] super::signing::Error),
    #[error(transparent)]
    Objects(#[from] super::review::Error),
}

pub struct PreparedBootstrapDocument {
    descriptor: GovernmentsDescriptor,
    body: PublicationBody,
    document: Vec<u8>,
    subject: RepositorySubject,
    metadata: Author,
}

pub struct PreparedBootstrapIdentity {
    document: PreparedBootstrapDocument,
    document_signature: BlsEnvelope,
    commit: UnsignedCommit,
    subject: RepositorySubject,
    head: git::Oid,
}

pub struct PreparedBootstrapRefs {
    body: PublicationBody,
    refs: Refs,
    prepared: PreparedSignedRefs,
    signatures: [BlsEnvelope; 2],
    subject: RepositorySubject,
}

/// The material has already been bound to the local finalized committee and public Output.
/// This only prepares bytes; the signing service must recheck current bootstrap permission.
pub fn prepare_bootstrap_document(
    descriptor: &GovernmentsDescriptor,
    material: &RepositorySigningMaterial,
) -> Result<PreparedBootstrapDocument, Error> {
    let authority = material.authority();
    descriptor
        .validate(authority.chain_id(), descriptor.genesis_timestamp)
        .map_err(|_| Error::Descriptor)?;
    let owner = BlsPublicKey::from_bytes(
        authority
            .committee()
            .vrf_group_public_key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| Error::Descriptor)?,
    )?;
    let (blob, document) = encode_document(descriptor, owner)?;
    let body = PublicationBody {
        chain_id: authority.chain_id(),
        genesis_hash: authority.genesis_hash(),
        repo_id: blob.into_sha1().ok_or(Error::Descriptor)?.into(),
        descriptor_hash: descriptor.hash().map_err(|_| Error::Descriptor)?,
        owner_bls_public: (*owner.as_bytes()).into(),
        players_commitment: material.players_commitment().into(),
        epoch: authority.epoch(),
        material_version: authority.committee().vrf_material_version,
        material_hash: material.material_hash().into(),
        finalized_anchor_height: 0,
        finalized_anchor_hash: authority.genesis_hash(),
        sequence: 1,
        previous_receipt_hash: B256::ZERO,
        action: PublicationAction::Bootstrap,
        proposal_id: U256::ZERO,
        patch_id: Default::default(),
        revision_id: Default::default(),
        expected_target_head: Default::default(),
        package_manifest_hash: B256::ZERO,
    };
    let subject = RepositorySubject::identity(
        body.repo_id.0,
        native_policy_context(&body, 0),
        body.repo_id.0,
    );
    let metadata = format!(
        "Outbe Network <network@outbe.invalid> {} +0000",
        descriptor.genesis_timestamp
    )
    .parse()
    .map_err(|_| Error::Descriptor)?;
    // Git timestamps are signed; fail before installing any objects on an unsupported genesis.
    i64::try_from(descriptor.genesis_timestamp).map_err(|_| Error::Descriptor)?;
    Ok(PreparedBootstrapDocument {
        descriptor: descriptor.clone(),
        body,
        document,
        subject,
        metadata,
    })
}

fn encode_document(
    descriptor: &GovernmentsDescriptor,
    owner: BlsPublicKey,
) -> Result<(git::Oid, Vec<u8>), Error> {
    let document = Doc::initial(
        Project::new(
            "governments".parse().map_err(|_| Error::Descriptor)?,
            descriptor.description.clone(),
            git::fmt::refname!("main"),
        )
        .map_err(|_| Error::Descriptor)?,
        RepositoryKey::from(owner).into(),
        Visibility::Public,
    );
    Ok(document.encode()?)
}

impl PreparedBootstrapDocument {
    pub fn repository_id(&self) -> RepoId {
        git::Oid::from_sha1(self.body.repo_id.0).into()
    }
    pub fn subject(&self) -> &RepositorySubject {
        &self.subject
    }
    pub fn body(&self) -> &PublicationBody {
        &self.body
    }

    /// The caller supplies the exact genesis manifest artifact and its committed Git tree.
    /// Neither object installation here nor signature attachment grants publication permission.
    pub fn attach(
        self,
        repo: &Repository,
        signature: &BlsEnvelope,
        content_manifest: &[u8],
    ) -> Result<PreparedBootstrapIdentity, Error> {
        if Sha256::hash(&[content_manifest]).0 != self.descriptor.initial_content_manifest_sha256.0
        {
            return Err(Error::Descriptor);
        }
        self.attach_native(repo, signature)
    }

    fn attach_native(
        self,
        repo: &Repository,
        signature: &BlsEnvelope,
    ) -> Result<PreparedBootstrapIdentity, Error> {
        if repo.id() != self.repository_id() {
            return Err(Error::Descriptor);
        }
        signature.verify(
            &self.subject,
            BlsPublicKey::from_bytes(self.body.owner_bls_public.0)?.into(),
        )?;
        let tree_oid = git::Oid::from_sha1(self.descriptor.initial_tree_oid.0);
        // Validate the whole committed content closure before creating the initial commit.
        object_closure(repo.raw(), vec![tree_oid], false)?;
        let tree = repo.raw().find_tree(tree_oid.into())?;
        let actor = git::raw::Signature::new(
            "Outbe Network",
            "network@outbe.invalid",
            &git::raw::Time::new(self.descriptor.genesis_timestamp as i64, 0),
        )?;
        let head = repo
            .raw()
            .commit(None, &actor, &actor, "Bootstrap repository\n", &tree, &[])?
            .into();
        let blob = git::Oid::from_sha1(self.body.repo_id.0);
        let tx = Transaction::<identity::Identity, Repository>::new(
            identity::TYPENAME.clone(),
            vec![identity::Action::Revision {
                title: cob::Title::new("Initial revision").map_err(|_| Error::Descriptor)?,
                description: String::new(),
                blob,
                parent: None,
                signature: signature.clone().into(),
            }],
            vec![cob::Embed::<cob::Uri>::store(
                "radicle.json",
                &self.document,
                repo.raw(),
            )?],
        );
        let commit = tx.prepare_native(
            repo,
            None,
            vec![],
            "Bootstrap repository\n".into(),
            self.metadata.clone(),
        )?;
        let subject = RepositorySubject::cob(
            self.body.repo_id.0,
            native_policy_context(&self.body, 1),
            commit.tree(),
            commit.as_bytes(),
        );
        Ok(PreparedBootstrapIdentity {
            document: self,
            document_signature: signature.clone(),
            commit,
            subject,
            head,
        })
    }
}

impl PreparedBootstrapIdentity {
    pub(crate) fn document_signature(&self) -> &BlsEnvelope {
        &self.document_signature
    }
    pub fn subject(&self) -> &RepositorySubject {
        &self.subject
    }
    pub fn body(&self) -> &PublicationBody {
        &self.document.body
    }
    pub fn attach(
        self,
        repo: &Repository,
        signature: &BlsEnvelope,
    ) -> Result<PreparedBootstrapRefs, Error> {
        if repo.id() != self.document.repository_id() {
            return Err(Error::Descriptor);
        }
        let owner: RepositoryKey =
            BlsPublicKey::from_bytes(self.document.body.owner_bls_public.0)?.into();
        signature.verify(&self.subject, owner)?;
        let identity = cob::attach_native(
            repo.raw(),
            self.commit,
            self.document.body.repo_id.0,
            native_policy_context(&self.document.body, 1),
            signature,
        )?
        .id;
        let mut refs = Refs::new();
        refs.insert(IDENTITY_ROOT.to_ref_string(), identity);
        refs.insert(
            git::fmt::qualified!("refs/rad/id").to_ref_string(),
            identity,
        );
        refs.insert(
            git::refs::storage::cob(&owner, &identity::TYPENAME, &identity.into())
                .strip_namespace_recursive()
                .to_ref_string(),
            identity,
        );
        refs.insert(
            git::fmt::qualified!("refs/heads/main").to_ref_string(),
            self.head,
        );
        let prepared = PreparedSignedRefs::new(
            refs.clone(),
            repo.id(),
            owner,
            native_policy_context(&self.document.body, 2),
            None,
            Committer::new(self.document.metadata),
            "Update signed refs\n".into(),
        )?;
        let subject = prepared.subject()?;
        Ok(PreparedBootstrapRefs {
            body: self.document.body,
            refs,
            prepared,
            signatures: [self.document_signature, signature.clone()],
            subject,
        })
    }
}

impl PreparedBootstrapRefs {
    pub(crate) fn signatures(&self) -> [&BlsEnvelope; 2] {
        [&self.signatures[0], &self.signatures[1]]
    }
    pub fn subject(&self) -> &RepositorySubject {
        &self.subject
    }
    pub fn body(&self) -> &PublicationBody {
        &self.body
    }
    pub fn attach(
        mut self,
        repo: &Repository,
        signature: &BlsEnvelope,
    ) -> Result<(PublicationBody, PublicationManifest), Error> {
        if repo.id().into_sha1() != Some(self.body.repo_id.0) {
            return Err(Error::Descriptor);
        }
        let sigrefs = self.prepared.attach(repo.raw(), signature.clone())?.oid();
        let roots = self
            .refs
            .iter()
            .map(|(_, oid)| *oid)
            .chain([sigrefs])
            .collect();
        let odb = repo.raw().odb()?;
        let objects = object_closure(repo.raw(), roots, false)?
            .into_iter()
            .map(|oid| {
                let object = odb.read(oid.into())?;
                let kind = match object.kind() {
                    git::raw::ObjectType::Blob => ObjectKind::Blob,
                    git::raw::ObjectType::Commit => ObjectKind::Commit,
                    git::raw::ObjectType::Tree => ObjectKind::Tree,
                    _ => return Err(Error::Descriptor),
                };
                Ok(ObjectCommitment {
                    kind,
                    oid: oid.to_string(),
                    sha256: hex::encode(Sha256::hash(&[object.data()]).0),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let mut updates: Vec<_> = self
            .refs
            .iter()
            .map(|(name, oid)| RefUpdate {
                name: name.to_string(),
                old: git::Oid::ZERO_SHA1.to_string(),
                new: oid.to_string(),
            })
            .collect();
        updates.push(RefUpdate {
            name: SIGREFS_BRANCH.to_string(),
            old: git::Oid::ZERO_SHA1.to_string(),
            new: sigrefs.to_string(),
        });
        let signatures = [&self.signatures[0], &self.signatures[1], signature];
        let manifest = PublicationManifest::new(
            objects,
            updates,
            signatures
                .into_iter()
                .enumerate()
                .map(|(ordinal, signature)| NativeSubject {
                    ordinal: ordinal as u8,
                    envelope: signature.to_hex(),
                })
                .collect(),
            self.refs
                .iter()
                .map(|(name, oid)| OwnerRef {
                    name: name.to_string(),
                    oid: oid.to_string(),
                })
                .collect(),
        )?;
        self.body.package_manifest_hash = manifest.hash()?;
        manifest.bind_receipt(&self.body)?;
        Ok((self.body, manifest))
    }
}

/// Full native/q-proof checked package, still requiring current permission and durable selection.
pub struct VerifiedBootstrapPublication {
    body: PublicationBody,
    manifest: PublicationManifest,
    witness_hash: B256,
    descriptor: GovernmentsDescriptor,
    content_manifest: Vec<u8>,
}

impl VerifiedBootstrapPublication {
    pub fn body(&self) -> &PublicationBody {
        &self.body
    }
    pub fn manifest(&self) -> &PublicationManifest {
        &self.manifest
    }
    pub(crate) fn witness_hash(&self) -> B256 {
        self.witness_hash
    }
    pub(crate) fn descriptor(&self) -> &GovernmentsDescriptor {
        &self.descriptor
    }
    pub(crate) fn content_manifest(&self) -> &[u8] {
        &self.content_manifest
    }
    pub fn authorize_current(
        &self,
        state: &crate::manager::FinalizedBootstrapSigningState,
        material: &RepositorySigningMaterial,
    ) -> Result<(), Error> {
        if material.authority() != state.authority() {
            return Err(Error::Descriptor);
        }
        let mut expected = prepare_bootstrap_document(state.descriptor(), material)?.body;
        expected.package_manifest_hash = self.manifest.hash()?;
        if expected != self.body {
            return Err(Error::Descriptor);
        }
        Ok(())
    }
}

pub struct BootstrapPublicationInput<'a> {
    pub state: &'a crate::manager::FinalizedBootstrapSigningState,
    pub material: &'a RepositorySigningMaterial,
    pub content_manifest: &'a [u8],
}

pub fn prepare_bootstrap_publication(
    repo: &Repository,
    input: BootstrapPublicationInput<'_>,
    signatures: [&BlsEnvelope; 3],
    witness: &super::signing::witness::RepositoryWitnessBundle,
) -> Result<VerifiedBootstrapPublication, Error> {
    let BootstrapPublicationInput {
        state,
        material,
        content_manifest,
    } = input;
    if material.authority() != state.authority() {
        return Err(Error::Descriptor);
    }
    let job = prepare_bootstrap_document(state.descriptor(), material)?;
    let (body, manifest) =
        reconstruct_bootstrap(repo, job, Some(content_manifest), signatures, |actual| {
            witness.verify(material, actual)
        })?;
    Ok(VerifiedBootstrapPublication {
        body,
        manifest,
        witness_hash: Sha256::hash(&[&witness.to_bytes()?]).0.into(),
        descriptor: state.descriptor().clone(),
        content_manifest: content_manifest.to_vec(),
    })
}

/// Reverify a persisted/received candidate; commitments alone do not authorize publication.
pub fn verify_bootstrap_publication(
    repo: &Repository,
    input: BootstrapPublicationInput<'_>,
    body: &PublicationBody,
    manifest: &PublicationManifest,
    witness: &super::signing::witness::RepositoryWitnessBundle,
) -> Result<VerifiedBootstrapPublication, Error> {
    manifest.bind_receipt(body)?;
    if manifest.subjects().len() != 3 {
        return Err(Error::Descriptor);
    }
    let signatures = manifest
        .subjects()
        .iter()
        .map(|entry| BlsEnvelope::from_hex(&entry.envelope))
        .collect::<Result<Vec<_>, _>>()?;
    let expected = prepare_bootstrap_publication(
        repo,
        input,
        [&signatures[0], &signatures[1], &signatures[2]],
        witness,
    )?;
    if expected.body != *body || expected.manifest != *manifest {
        return Err(Error::Descriptor);
    }
    Ok(expected)
}

fn reconstruct_bootstrap(
    repo: &Repository,
    job: PreparedBootstrapDocument,
    content_manifest: Option<&[u8]>,
    signatures: [&BlsEnvelope; 3],
    verify: impl FnOnce(&[(RepositorySubject, BlsEnvelope)]) -> Result<(), super::signing::Error>,
) -> Result<(PublicationBody, PublicationManifest), Error> {
    let mut actual = vec![(job.subject().clone(), signatures[0].clone())];
    let identity = match content_manifest {
        Some(content) => job.attach(repo, signatures[0], content)?,
        None => job.attach_native(repo, signatures[0])?,
    };
    actual.push((identity.subject().clone(), signatures[1].clone()));
    let refs = identity.attach(repo, signatures[1])?;
    actual.push((refs.subject().clone(), signatures[2].clone()));
    verify(&actual)?;
    refs.attach(repo, signatures[2])
}

/// Exact accepted bootstrap only; cannot authorize a new signature or a different root.
pub struct AcceptedBootstrapPublication {
    receipt: crate::manager::FinalizedPublicationReceipt,
    manifest: PublicationManifest,
}

impl AcceptedBootstrapPublication {
    /// All objects/signatures have been reconstructed against an exact accepted chain receipt.
    /// Lock the entire owner snapshot before CAS; already-new refs recover an interrupted install.
    pub fn install(&self, repo: &Repository) -> Result<(), Error> {
        let body = &self.receipt.receipt().body;
        if repo.id().into_sha1() != Some(body.repo_id.0) {
            return Err(Error::Descriptor);
        }
        super::publication::install_owner_refs(repo, body, &self.manifest).map_err(|error| {
            match error {
                super::publication::Error::RefConflict => Error::RefConflict,
                super::publication::Error::Git(error) => Error::Git(error),
                error => Error::Manifest(error),
            }
        })?;
        let identity = self
            .manifest
            .owner_refs()?
            .get(&IDENTITY_ROOT)
            .ok_or(Error::Descriptor)?;
        repo.set_identity_head_to(identity)?;
        repo.set_default_branch_to_canonical_head()?;
        repo.raw().set_head("refs/heads/main")?;
        Ok(())
    }

    pub fn receipt(&self) -> &crate::manager::FinalizedPublicationReceipt {
        &self.receipt
    }
    pub fn manifest(&self) -> &PublicationManifest {
        &self.manifest
    }
}

/// Reconstruct all native objects and signatures against an exact trusted receipt.
/// This is historical verification; current material rotation does not revoke the package.
pub fn verify_accepted_bootstrap(
    repo: &Repository,
    descriptor: &GovernmentsDescriptor,
    verifier: &super::signing::accepted::AcceptedPublicationVerifier,
) -> Result<AcceptedBootstrapPublication, Error> {
    let receipt = verifier.receipt();
    let body = &receipt.receipt().body;
    if verifier.manifest().subjects().len() != 3 {
        return Err(Error::Descriptor);
    }
    let job = accepted_document(descriptor, body)?;

    let signatures = verifier
        .manifest()
        .subjects()
        .iter()
        .map(|subject| BlsEnvelope::from_hex(&subject.envelope))
        .collect::<Result<Vec<_>, _>>()?;
    let (expected, manifest) = reconstruct_bootstrap(
        repo,
        job,
        None,
        [&signatures[0], &signatures[1], &signatures[2]],
        |actual| verifier.verify_native(actual),
    )?;
    if expected != *body || manifest != *verifier.manifest() {
        return Err(Error::Descriptor);
    }
    Ok(AcceptedBootstrapPublication {
        receipt: receipt.clone(),
        manifest,
    })
}

pub(super) fn recover_bootstrap_manifest(
    repo: &Repository,
    descriptor: &GovernmentsDescriptor,
    body: &PublicationBody,
    signatures: [&BlsEnvelope; 3],
) -> Result<PublicationManifest, Error> {
    let (_, manifest) = reconstruct_bootstrap(
        repo,
        accepted_document(descriptor, body)?,
        None,
        signatures,
        |_| Ok(()),
    )?;
    manifest.bind_receipt(body)?;
    Ok(manifest)
}

fn accepted_document(
    descriptor: &GovernmentsDescriptor,
    body: &PublicationBody,
) -> Result<PreparedBootstrapDocument, Error> {
    descriptor
        .validate(body.chain_id, descriptor.genesis_timestamp)
        .map_err(|_| Error::Descriptor)?;
    if body.action != PublicationAction::Bootstrap
        || body.sequence != 1
        || !body.previous_receipt_hash.is_zero()
    {
        return Err(Error::Descriptor);
    }
    if !body.proposal_id.is_zero()
        || !body.patch_id.is_zero()
        || !body.revision_id.is_zero()
        || !body.expected_target_head.is_zero()
    {
        return Err(Error::Descriptor);
    }
    if body.finalized_anchor_height != 0
        || body.finalized_anchor_hash != body.genesis_hash
        || body.descriptor_hash != descriptor.hash().map_err(|_| Error::Descriptor)?
    {
        return Err(Error::Descriptor);
    }
    let owner = BlsPublicKey::from_bytes(body.owner_bls_public.0)?;
    let (blob, document) = encode_document(descriptor, owner)?;
    if blob.into_sha1() != Some(body.repo_id.0) {
        return Err(Error::Descriptor);
    }
    i64::try_from(descriptor.genesis_timestamp).map_err(|_| Error::Descriptor)?;
    let subject = RepositorySubject::identity(
        body.repo_id.0,
        native_policy_context(body, 0),
        body.repo_id.0,
    );
    let metadata = format!(
        "Outbe Network <network@outbe.invalid> {} +0000",
        descriptor.genesis_timestamp
    )
    .parse()
    .map_err(|_| Error::Descriptor)?;
    Ok(PreparedBootstrapDocument {
        descriptor: descriptor.clone(),
        body: body.clone(),
        document,
        subject,
        metadata,
    })
}

#[cfg(test)]
mod tests;
