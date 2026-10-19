//! Durable publication commitments, immutable Review packages and local attestation selection.

use super::{
    review::VerifiedReviewPublication, CheckpointOwnerSnapshot, ObjectKind, PublicationManifest,
    MAX_MANIFEST_BYTES,
};
use crate::{
    integration::signing::{
        witness::{RepositoryWitnessBundle, MAX_WITNESS_BYTES},
        RepositorySigningMaterial,
    },
    manager::{FinalizedPatchSigningState, FinalizedPublicationReceipt, RepositoryReceiptReader},
};
use alloy_primitives::B256;
use commonware_cryptography::{Hasher as _, Sha256};
use outbe_primitives::{
    radicle_genesis::{GovernmentsDescriptor, MAX_GOVERNMENTS_DESCRIPTOR_BYTES},
    radicle_publication::{PublicationAction, PublicationBody, PUBLICATION_BODY_LEN},
};
use radicle::{
    git,
    storage::{git::Repository, WriteRepository as _},
};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
};

const READY: &[u8] = b"OPR1";
const SLOT: &[u8] = b"OAS1";
const MAX_OBJECT_BYTES: usize = radicle::crypto::repository::MAX_NATIVE_PAYLOAD;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Git(#[from] git::raw::Error),
    #[error(transparent)]
    Manifest(#[from] super::Error),
    #[error(transparent)]
    Witness(#[from] crate::integration::signing::Error),
    #[error(transparent)]
    Authority(#[from] crate::manager::ManagerError),
    #[error("accepted publication checkpoint {0} is unavailable")]
    MissingCheckpoint(u64),
    #[error("publication store is already owned or cannot be locked: {0}")]
    Ownership(String),
    #[error("invalid or corrupt persisted Review package")]
    Corrupt,
    #[error("another body is already selected for this attestation slot")]
    SlotConflict,
}

pub struct ReviewPackageStore {
    root: PathBuf,
    _ownership: File,
}

/// This token proves persistence/selection, not current permission; the actor rechecks that.
pub struct SelectedReviewPublication {
    publication: SelectedPatch,
}

enum SelectedPatch {
    Review(Arc<VerifiedReviewPublication>),
    Merge(Arc<super::merge::VerifiedMergePublication>),
    Material(Arc<super::binding::VerifiedMaterialPublication>),
}

impl SelectedPatch {
    fn body(&self) -> &PublicationBody {
        match self {
            Self::Review(p) => p.body(),
            Self::Merge(p) => p.body(),
            Self::Material(p) => p.body(),
        }
    }

    fn manifest(&self) -> &PublicationManifest {
        match self {
            Self::Review(p) => p.manifest(),
            Self::Merge(p) => p.manifest(),
            Self::Material(p) => p.manifest(),
        }
    }

    fn witness_hash(&self) -> B256 {
        match self {
            Self::Review(p) => p.witness_hash(),
            Self::Merge(p) => p.witness_hash(),
            Self::Material(p) => p.witness_hash(),
        }
    }
}

impl SelectedReviewPublication {
    pub fn body(&self) -> &PublicationBody {
        self.publication.body()
    }
    pub fn manifest(&self) -> &PublicationManifest {
        self.publication.manifest()
    }
    pub fn authorize_current(
        &self,
        state: &FinalizedPatchSigningState,
        material: &RepositorySigningMaterial,
    ) -> Result<(), super::review::Error> {
        match &self.publication {
            SelectedPatch::Review(p) => p.authorize_current(state, material),
            SelectedPatch::Merge(p) => p.authorize_current(state, material),
            SelectedPatch::Material(_) => Err(super::review::Error::Context),
        }
    }
    pub fn authorize_material(
        &self,
        state: &crate::manager::FinalizedMaterialSigningState,
        material: &RepositorySigningMaterial,
    ) -> Result<(), super::review::Error> {
        match &self.publication {
            SelectedPatch::Material(p) => p.authorize_current(state, material),
            _ => Err(super::review::Error::Context),
        }
    }
}

pub struct SelectedBootstrapPublication {
    publication: Arc<crate::integration::bootstrap::VerifiedBootstrapPublication>,
}

impl SelectedBootstrapPublication {
    pub fn body(&self) -> &PublicationBody {
        self.publication.body()
    }
    pub fn manifest(&self) -> &PublicationManifest {
        self.publication.manifest()
    }
    pub fn authorize_current(
        &self,
        state: &crate::manager::FinalizedBootstrapSigningState,
        material: &RepositorySigningMaterial,
    ) -> Result<(), crate::integration::bootstrap::Error> {
        self.publication.authorize_current(state, material)
    }
}

/// Loaded bytes still require native package verification or an authenticated accepted receipt.
pub struct StoredReviewPackage {
    pub body: PublicationBody,
    pub manifest: PublicationManifest,
    pub witness: RepositoryWitnessBundle,
}

/// Receipt-bound history has no signing witness or local attestation selection.
pub struct StoredAcceptedPackage {
    pub body: PublicationBody,
    pub manifest: PublicationManifest,
}

/// Local slot isolation includes the chain/repo and the canonical bound material.
/// Interchangeable witnesses and candidate-specific fields do not create another slot.
pub fn review_slot_key(body: &PublicationBody) -> B256 {
    Sha256::hash(&[
        b"OLS1",
        &body.chain_id.to_be_bytes(),
        body.genesis_hash.as_slice(),
        body.repo_id.as_slice(),
        &body.epoch.to_be_bytes(),
        &body.material_version.to_be_bytes(),
        body.material_hash.as_slice(),
        body.players_commitment.as_slice(),
        &body.sequence.to_be_bytes(),
    ])
    .0
    .into()
}

impl ReviewPackageStore {
    /// Use a node-local private directory, on a blocking worker. The OS lock survives neither
    /// process exit nor crash, while the immutable selected-body record survives both.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, Error> {
        let root = if root.as_ref().is_absolute() {
            root.as_ref().to_path_buf()
        } else {
            std::env::current_dir()?.join(root)
        };
        private_dir(&root)?;
        let lock = root.join("writer.lock");
        reject_symlink(&lock)?;
        let ownership = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock)?;
        ownership
            .try_lock()
            .map_err(|error| Error::Ownership(error.to_string()))?;
        for child in ["packages", "slots"] {
            private_dir(&root.join(child))?;
        }
        Ok(Self {
            root,
            _ownership: ownership,
        })
    }

    /// Cache only commitments already bound to a local finalized receipt. This creates neither
    /// a ready package/selection nor native entry grants; those require complete verification.
    pub fn persist_checkpoint_manifest(
        &mut self,
        checkpoint: &FinalizedPublicationReceipt,
        manifest: &PublicationManifest,
    ) -> Result<(), Error> {
        let receipt = checkpoint.receipt();
        ensure_corrupt(receipt.receipt_hash == receipt.body.receipt_hash())?;
        manifest.bind_receipt(&receipt.body)?;
        let directory = self.package_dir(receipt.receipt_hash);
        private_dir(&directory)?;
        write_once(&directory.join("body"), &receipt.body.to_bytes())?;
        write_once(&directory.join("manifest"), &manifest.to_bytes()?)
    }

    pub fn persist_accepted_descriptor(
        &self,
        checkpoint: &FinalizedPublicationReceipt,
        descriptor: &GovernmentsDescriptor,
    ) -> Result<(), Error> {
        let body = &checkpoint.receipt().body;
        descriptor
            .validate(body.chain_id, descriptor.genesis_timestamp)
            .map_err(|_| Error::Corrupt)?;
        ensure_corrupt(body.action == PublicationAction::Bootstrap)?;
        ensure_corrupt(body.sequence == 1)?;
        ensure_corrupt(checkpoint.receipt().receipt_hash == body.receipt_hash())?;
        ensure_corrupt(descriptor.hash().map_err(|_| Error::Corrupt)? == body.descriptor_hash)?;
        let directory = self.package_dir(checkpoint.receipt().receipt_hash);
        private_dir(&directory)?;
        write_once(
            &directory.join("descriptor"),
            &descriptor.to_bytes().map_err(|_| Error::Corrupt)?,
        )
    }

    pub fn load_checkpoint_manifest(
        &self,
        checkpoint: &FinalizedPublicationReceipt,
    ) -> Result<PublicationManifest, Error> {
        let receipt = checkpoint.receipt();
        let directory = self.package_dir(receipt.receipt_hash);
        reject_symlink(&directory)?;
        ensure_corrupt(receipt.receipt_hash == receipt.body.receipt_hash())?;
        ensure_corrupt(
            read(&directory.join("body"), PUBLICATION_BODY_LEN)? == receipt.body.to_bytes(),
        )?;
        let manifest = PublicationManifest::from_bytes(&read(
            &directory.join("manifest"),
            MAX_MANIFEST_BYTES,
        )?)?;
        manifest.bind_receipt(&receipt.body)?;
        Ok(manifest)
    }

    /// Rebuild preparation refs in receipt order on a blocking worker. Durable bytes alone never
    /// confer authority: every row is reread from this node's canonical finalized state.
    pub fn replay_checkpoint_snapshot(
        &self,
        reader: &dyn RepositoryReceiptReader,
        target: &FinalizedPublicationReceipt,
    ) -> Result<CheckpointOwnerSnapshot, Error> {
        let sequence = target.receipt().body.sequence;
        ensure_corrupt(sequence != 0)?;
        let mut snapshot: Option<CheckpointOwnerSnapshot> = None;
        for expected in 1..=sequence {
            let checkpoint = reader
                .read_current_publication_receipt(expected)?
                .ok_or(Error::MissingCheckpoint(expected))?;
            ensure_corrupt(checkpoint.receipt().body.sequence == expected)?;
            ensure_corrupt(expected != sequence || checkpoint.receipt() == target.receipt())?;
            let manifest = self.load_checkpoint_manifest(&checkpoint)?;
            match &mut snapshot {
                None => snapshot = Some(CheckpointOwnerSnapshot::bootstrap(checkpoint, &manifest)?),
                Some(snapshot) => snapshot.advance(checkpoint, &manifest)?,
            }
        }
        snapshot.ok_or(Error::Corrupt)
    }

    /// Persist the complete verified package before making its body eligible for attestation.
    pub fn persist_and_select(
        &mut self,
        repo: &Repository,
        publication: Arc<VerifiedReviewPublication>,
        witness: &RepositoryWitnessBundle,
    ) -> Result<SelectedReviewPublication, Error> {
        self.persist_and_select_patch(repo, witness, SelectedPatch::Review(publication))
    }

    pub fn persist_and_select_merge(
        &mut self,
        repo: &Repository,
        publication: Arc<super::merge::VerifiedMergePublication>,
        witness: &RepositoryWitnessBundle,
    ) -> Result<SelectedReviewPublication, Error> {
        self.persist_and_select_patch(repo, witness, SelectedPatch::Merge(publication))
    }

    pub fn persist_and_select_bootstrap(
        &mut self,
        repo: &Repository,
        publication: Arc<crate::integration::bootstrap::VerifiedBootstrapPublication>,
        witness: &RepositoryWitnessBundle,
    ) -> Result<SelectedBootstrapPublication, Error> {
        self.persist(
            repo,
            PersistPackage {
                body: publication.body(),
                manifest: publication.manifest(),
                expected_witness_hash: publication.witness_hash(),
                witness,
                bootstrap: Some((publication.descriptor(), publication.content_manifest())),
            },
        )?;
        Ok(SelectedBootstrapPublication { publication })
    }

    pub fn persist_and_select_material(
        &mut self,
        repo: &Repository,
        publication: Arc<super::binding::VerifiedMaterialPublication>,
        witness: &RepositoryWitnessBundle,
    ) -> Result<SelectedReviewPublication, Error> {
        self.persist_and_select_patch(repo, witness, SelectedPatch::Material(publication))
    }

    fn persist_and_select_patch(
        &mut self,
        repo: &Repository,
        witness: &RepositoryWitnessBundle,
        publication: SelectedPatch,
    ) -> Result<SelectedReviewPublication, Error> {
        self.persist(
            repo,
            PersistPackage {
                body: publication.body(),
                manifest: publication.manifest(),
                expected_witness_hash: publication.witness_hash(),
                witness,
                bootstrap: None,
            },
        )?;
        Ok(SelectedReviewPublication { publication })
    }

    fn persist(&mut self, repo: &Repository, package: PersistPackage<'_>) -> Result<(), Error> {
        let PersistPackage {
            body,
            manifest,
            expected_witness_hash,
            witness,
            bootstrap,
        } = package;
        let body_hash = body.receipt_hash();
        let witness_bytes = witness.to_bytes()?;
        let witness_hash: B256 = Sha256::hash(&[&witness_bytes]).0.into();
        ensure_corrupt(witness_hash == expected_witness_hash)?;
        let slot = review_slot_key(body);
        if let Some((selected_body, selected_witness)) = self.selection(slot)? {
            if selected_body != body_hash {
                return Err(Error::SlotConflict);
            }
            // The durable pointer keeps its original witness even if a new valid subset arrives.
            self.load(selected_body, selected_witness, repo)?;
        }
        let directory = self.package_dir(body_hash);
        private_dir(&directory)?;
        for child in ["objects", "witnesses", "ready"] {
            private_dir(&directory.join(child))?;
        }
        write_once(&directory.join("body"), &body.to_bytes())?;
        write_once(&directory.join("manifest"), &manifest.to_bytes()?)?;
        if let Some((descriptor, content)) = bootstrap {
            validate_bootstrap_inputs(body, descriptor, content)?;
            write_once(
                &directory.join("descriptor"),
                &descriptor.to_bytes().map_err(|_| Error::Corrupt)?,
            )?;
            write_once(&directory.join("initial-content.manifest"), content)?;
        }
        let odb = repo.raw().odb()?;
        for row in manifest.objects() {
            let oid: git::Oid = row.oid.parse().map_err(|_| Error::Corrupt)?;
            let (size, kind) = odb.read_header(oid.into())?;
            ensure_corrupt(size <= MAX_OBJECT_BYTES)?;
            ensure_corrupt(kind == object_type(row.kind))?;
            let object = odb.read(oid.into())?;
            verify_object_digest(
                object.kind(),
                object.data(),
                object_type(row.kind),
                &row.sha256,
                oid,
            )?;
            write_once(&directory.join("objects").join(&row.sha256), object.data())?;
        }
        write_once(
            &directory.join("witnesses").join(hex::encode(witness_hash)),
            &witness_bytes,
        )?;
        write_once(
            &directory.join("ready").join(hex::encode(witness_hash)),
            READY,
        )?;
        self.select(slot, body_hash, witness_hash)?;
        Ok(())
    }

    /// Historical reconstruction inputs are authenticated by this node's accepted checkpoint.
    pub fn load_bootstrap_inputs(
        &self,
        checkpoint: &FinalizedPublicationReceipt,
    ) -> Result<(GovernmentsDescriptor, Vec<u8>), Error> {
        self.load_checkpoint_manifest(checkpoint)?;
        let body = &checkpoint.receipt().body;
        let directory = self.package_dir(checkpoint.receipt().receipt_hash);
        let descriptor = GovernmentsDescriptor::from_bytes(&read(
            &directory.join("descriptor"),
            MAX_GOVERNMENTS_DESCRIPTOR_BYTES,
        )?)
        .map_err(|_| Error::Corrupt)?;
        let content = read(
            &directory.join("initial-content.manifest"),
            MAX_MANIFEST_BYTES,
        )?;
        validate_bootstrap_inputs(body, &descriptor, &content)?;
        Ok((descriptor, content))
    }

    /// Rebuild read grants through the requested accepted checkpoint from own receipts and native bytes.
    /// Historical grants do not promote refs or authorize a new operation.
    pub fn open_checkpoint_repository(
        &self,
        reader: &dyn RepositoryReceiptReader,
        target: &FinalizedPublicationReceipt,
        path: &Path,
        info: git::UserInfo,
    ) -> Result<Repository, Error> {
        use radicle::storage::ReadStorage as _;
        let checkpoint = reader
            .read_current_publication_receipt(1)?
            .ok_or(Error::MissingCheckpoint(1))?;
        let body = &checkpoint.receipt().body;
        let id = git::Oid::from_sha1(body.repo_id.0).into();
        let storage = radicle::Storage::open(path.parent().ok_or(Error::Corrupt)?, info)
            .map_err(|_| Error::Corrupt)?;
        let repo = storage.repository(id).map_err(|_| Error::Corrupt)?;
        ensure_corrupt(repo.raw().path().canonicalize()? == path.canonicalize()?)?;
        let package = self
            .load_accepted(&checkpoint, &repo)?
            .ok_or(Error::Corrupt)?;
        ensure_corrupt(package.body == *body)?;
        let descriptor = self.resolve_accepted_descriptor(reader, &checkpoint)?;
        let verifier = super::super::signing::accepted::AcceptedPublicationVerifier::from_receipt(
            checkpoint.clone(),
            package.manifest.clone(),
        )?;
        let proof =
            crate::integration::bootstrap::verify_accepted_bootstrap(&repo, &descriptor, &verifier)
                .map_err(|_| Error::Corrupt)?;
        let mut snapshot =
            CheckpointOwnerSnapshot::bootstrap(checkpoint.clone(), &package.manifest)?;
        let policy = Arc::new(
            super::policy::AcceptedRepositoryPolicy::new(&snapshot).map_err(|_| Error::Corrupt)?,
        );
        policy
            .accept_bootstrap(&proof)
            .map_err(|_| Error::Corrupt)?;
        let owner = radicle::crypto::BlsPublicKey::from_bytes(body.owner_bls_public.0)
            .map_err(|_| Error::Corrupt)?;
        storage
            .manage_repository(id, owner.into(), policy.clone())
            .map_err(|_| Error::Corrupt)?;
        let repo = storage.repository(id).map_err(|_| Error::Corrupt)?;
        for sequence in 2..=target.receipt().body.sequence {
            self.replay_accepted_step(reader, &repo, &policy, &mut snapshot, sequence)?;
        }
        ensure_corrupt(snapshot.checkpoint().receipt() == target.receipt())?;
        Ok(repo)
    }

    fn resolve_accepted_descriptor(
        &self,
        reader: &dyn RepositoryReceiptReader,
        checkpoint: &FinalizedPublicationReceipt,
    ) -> Result<GovernmentsDescriptor, Error> {
        match self.load_accepted_descriptor(checkpoint) {
            Ok(descriptor) => Ok(descriptor),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                let descriptor = reader
                    .read_governments_descriptor()?
                    .ok_or(Error::Corrupt)?;
                self.persist_accepted_descriptor(checkpoint, &descriptor)?;
                Ok(descriptor)
            }
            Err(error) => Err(error),
        }
    }

    fn replay_accepted_step(
        &self,
        reader: &dyn RepositoryReceiptReader,
        repo: &Repository,
        policy: &super::policy::AcceptedRepositoryPolicy,
        snapshot: &mut CheckpointOwnerSnapshot,
        sequence: u64,
    ) -> Result<(), Error> {
        let checkpoint = reader
            .read_current_publication_receipt(sequence)?
            .ok_or(Error::MissingCheckpoint(sequence))?;
        let package = self
            .load_accepted(&checkpoint, repo)?
            .ok_or(Error::Corrupt)?;
        ensure_corrupt(
            package.body == checkpoint.receipt().body && package.body.sequence == sequence,
        )?;
        let verifier = super::super::signing::accepted::AcceptedPublicationVerifier::from_receipt(
            checkpoint.clone(),
            package.manifest.clone(),
        )?;
        match package.body.action {
            PublicationAction::Review => {
                let approval = reader
                    .read_current_patch_approval(package.body.proposal_id)?
                    .ok_or(Error::Corrupt)?;
                let proof = super::review::verify_accepted_review_publication(
                    repo, &approval, snapshot, &verifier,
                )
                .map_err(|_| Error::Corrupt)?;
                policy.accept_review(&proof).map_err(|_| Error::Corrupt)?;
            }
            PublicationAction::Merge => {
                let approval = reader
                    .read_current_patch_approval(package.body.proposal_id)?
                    .ok_or(Error::Corrupt)?;
                let review = self.load_accepted_review(
                    reader,
                    repo,
                    package.body.proposal_id,
                    snapshot.checkpoint(),
                )?;
                let proof = super::merge::verify_accepted_merge_publication(
                    repo, &approval, snapshot, &review, &verifier,
                )
                .map_err(|_| Error::Corrupt)?;
                policy.accept_merge(&proof).map_err(|_| Error::Corrupt)?;
            }
            PublicationAction::BindMaterial => {
                let proof =
                    super::binding::verify_accepted_material_publication(snapshot, &verifier)
                        .map_err(|_| Error::Corrupt)?;
                policy.accept_material(&proof).map_err(|_| Error::Corrupt)?;
            }
            _ => return Err(Error::Corrupt),
        }
        snapshot.advance(checkpoint, &package.manifest)?;
        Ok(())
    }

    pub fn load_accepted_review(
        &self,
        reader: &dyn RepositoryReceiptReader,
        repo: &Repository,
        proposal: alloy_primitives::U256,
        through: &FinalizedPublicationReceipt,
    ) -> Result<super::review::AcceptedReviewPublication, Error> {
        for sequence in (2..=through.receipt().body.sequence).rev() {
            let receipt = reader
                .read_current_publication_receipt(sequence)?
                .ok_or(Error::MissingCheckpoint(sequence))?;
            if receipt.receipt().body.action != PublicationAction::Review
                || receipt.receipt().body.proposal_id != proposal
            {
                continue;
            }
            let before = reader
                .read_current_publication_receipt(sequence - 1)?
                .ok_or(Error::MissingCheckpoint(sequence - 1))?;
            let snapshot = self.replay_checkpoint_snapshot(reader, &before)?;
            let package = self.load_accepted(&receipt, repo)?.ok_or(Error::Corrupt)?;
            ensure_corrupt(package.body == receipt.receipt().body)?;
            let approval = reader
                .read_current_patch_approval(proposal)?
                .ok_or(Error::Corrupt)?;
            let verifier =
                super::super::signing::accepted::AcceptedPublicationVerifier::from_receipt(
                    receipt,
                    package.manifest,
                )?;
            return super::review::verify_accepted_review_publication(
                repo, &approval, &snapshot, &verifier,
            )
            .map_err(|_| Error::Corrupt);
        }
        Err(Error::Corrupt)
    }

    /// Read exact accepted history independently of this node's participation in signing.
    pub fn load_accepted(
        &self,
        checkpoint: &FinalizedPublicationReceipt,
        repo: &Repository,
    ) -> Result<Option<StoredAcceptedPackage>, Error> {
        let directory = self.package_dir(checkpoint.receipt().receipt_hash);
        reject_symlink(&directory)?;
        if !directory.join("manifest").try_exists()? {
            return Ok(None);
        }
        let manifest = self.load_checkpoint_manifest(checkpoint)?;
        for row in manifest.objects() {
            let oid: git::Oid = row.oid.parse().map_err(|_| Error::Corrupt)?;
            let kind = object_type(row.kind);
            let Some(bytes) =
                read_or_load_accepted_object(repo, &directory, &row.sha256, oid, kind)?
            else {
                return Ok(None);
            };
            write_verified_object(repo, kind, &bytes, &row.sha256, oid)?;
        }
        Ok(Some(StoredAcceptedPackage {
            body: checkpoint.receipt().body.clone(),
            manifest,
        }))
    }

    /// The descriptor is committed by genesis and the accepted root; creation artifacts are not read.
    pub fn load_accepted_descriptor(
        &self,
        checkpoint: &FinalizedPublicationReceipt,
    ) -> Result<GovernmentsDescriptor, Error> {
        let body = &checkpoint.receipt().body;
        let directory = self.package_dir(checkpoint.receipt().receipt_hash);
        let descriptor = GovernmentsDescriptor::from_bytes(&read(
            &directory.join("descriptor"),
            MAX_GOVERNMENTS_DESCRIPTOR_BYTES,
        )?)
        .map_err(|_| Error::Corrupt)?;
        ensure_corrupt(body.action == PublicationAction::Bootstrap)?;
        ensure_corrupt(descriptor.hash().map_err(|_| Error::Corrupt)? == body.descriptor_hash)?;
        Ok(descriptor)
    }

    pub fn load_selected(
        &self,
        slot: B256,
        repo: &Repository,
    ) -> Result<Option<StoredReviewPackage>, Error> {
        let Some((body, witness)) = self.selection(slot)? else {
            return Ok(None);
        };
        self.load(body, witness, repo).map(Some)
    }

    fn load(
        &self,
        body_hash: B256,
        witness_hash: B256,
        repo: &Repository,
    ) -> Result<StoredReviewPackage, Error> {
        let directory = self.package_dir(body_hash);
        let ready = read(
            &directory.join("ready").join(hex::encode(witness_hash)),
            READY.len(),
        )?;
        ensure_corrupt(ready == READY)?;
        let body =
            PublicationBody::from_bytes(&read(&directory.join("body"), PUBLICATION_BODY_LEN)?)
                .map_err(|_| Error::Corrupt)?;
        let manifest = PublicationManifest::from_bytes(&read(
            &directory.join("manifest"),
            MAX_MANIFEST_BYTES,
        )?)?;
        let bytes = read(
            &directory.join("witnesses").join(hex::encode(witness_hash)),
            MAX_WITNESS_BYTES,
        )?;
        ensure_corrupt(body.receipt_hash() == body_hash)?;
        ensure_corrupt(manifest.hash()? == body.package_manifest_hash)?;
        ensure_corrupt(B256::from(Sha256::hash(&[&bytes]).0) == witness_hash)?;
        let witness = RepositoryWitnessBundle::from_bytes(&bytes)?;
        for row in manifest.objects() {
            let bytes = read(
                &directory.join("objects").join(&row.sha256),
                MAX_OBJECT_BYTES,
            )?;
            let oid: git::Oid = row.oid.parse().map_err(|_| Error::Corrupt)?;
            write_verified_object(repo, object_type(row.kind), &bytes, &row.sha256, oid)?;
        }
        Ok(StoredReviewPackage {
            body,
            manifest,
            witness,
        })
    }

    fn package_dir(&self, body: B256) -> PathBuf {
        self.root.join("packages").join(hex::encode(body))
    }

    fn selection(&self, slot: B256) -> Result<Option<(B256, B256)>, Error> {
        let path = self.root.join("slots").join(hex::encode(slot));
        let Some(bytes) = read_optional(&path, 68)? else {
            return Ok(None);
        };
        ensure_corrupt(bytes.strip_prefix(SLOT).map(<[u8]>::len) == Some(64))?;
        Ok(Some((
            B256::from_slice(&bytes[4..36]),
            B256::from_slice(&bytes[36..]),
        )))
    }

    fn select(&mut self, slot: B256, body: B256, witness: B256) -> Result<(), Error> {
        if let Some((selected, _)) = self.selection(slot)? {
            return (selected == body).then_some(()).ok_or(Error::SlotConflict);
        }
        write_once(
            &self.root.join("slots").join(hex::encode(slot)),
            &[SLOT, body.as_slice(), witness.as_slice()].concat(),
        )
    }
}

struct PersistPackage<'a> {
    body: &'a PublicationBody,
    manifest: &'a PublicationManifest,
    expected_witness_hash: B256,
    witness: &'a RepositoryWitnessBundle,
    bootstrap: Option<(&'a GovernmentsDescriptor, &'a [u8])>,
}

fn ensure_corrupt(condition: bool) -> Result<(), Error> {
    condition.then_some(()).ok_or(Error::Corrupt)
}

fn verify_object_digest(
    actual_kind: git::raw::ObjectType,
    bytes: &[u8],
    expected_kind: git::raw::ObjectType,
    expected_sha256: &str,
    expected_oid: git::Oid,
) -> Result<(), Error> {
    ensure_corrupt(actual_kind == expected_kind)?;
    ensure_corrupt(bytes.len() <= MAX_OBJECT_BYTES)?;
    ensure_corrupt(hex::encode(Sha256::hash(&[bytes]).0) == expected_sha256)?;
    ensure_corrupt(git::raw::Oid::hash_object(expected_kind, bytes)? == expected_oid.into())
}

fn write_verified_object(
    repo: &Repository,
    kind: git::raw::ObjectType,
    bytes: &[u8],
    expected_sha256: &str,
    expected_oid: git::Oid,
) -> Result<(), Error> {
    verify_object_digest(kind, bytes, kind, expected_sha256, expected_oid)?;
    ensure_corrupt(repo.raw().odb()?.write(kind, bytes)? == expected_oid.into())
}

fn read_or_load_accepted_object(
    repo: &Repository,
    directory: &Path,
    sha256: &str,
    oid: git::Oid,
    kind: git::raw::ObjectType,
) -> Result<Option<Vec<u8>>, Error> {
    let odb = repo.raw().odb()?;
    match odb.read_header(oid.into()) {
        Ok((size, actual_kind)) => {
            ensure_corrupt(size <= MAX_OBJECT_BYTES)?;
            ensure_corrupt(actual_kind == kind)?;
            Ok(Some(odb.read(oid.into())?.data().to_vec()))
        }
        Err(error) if error.code() == git::raw::ErrorCode::NotFound => {
            read_optional(&directory.join("objects").join(sha256), MAX_OBJECT_BYTES)
        }
        Err(error) => Err(error.into()),
    }
}

fn validate_bootstrap_inputs(
    body: &PublicationBody,
    descriptor: &GovernmentsDescriptor,
    content: &[u8],
) -> Result<(), Error> {
    ensure_corrupt(body.action == PublicationAction::Bootstrap)?;
    ensure_corrupt(body.sequence == 1)?;
    ensure_corrupt(body.previous_receipt_hash.is_zero())?;
    ensure_corrupt(descriptor.chain_id == body.chain_id)?;
    ensure_corrupt(descriptor.hash().map_err(|_| Error::Corrupt)? == body.descriptor_hash)?;
    ensure_corrupt(content.len() <= MAX_MANIFEST_BYTES)?;
    ensure_corrupt(
        B256::from(Sha256::hash(&[content]).0) == descriptor.initial_content_manifest_sha256,
    )
}

fn object_type(kind: ObjectKind) -> git::raw::ObjectType {
    match kind {
        ObjectKind::Blob => git::raw::ObjectType::Blob,
        ObjectKind::Tree => git::raw::ObjectType::Tree,
        ObjectKind::Commit => git::raw::ObjectType::Commit,
    }
}

fn reject_symlink(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure_corrupt(!metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn private_dir(path: &Path) -> Result<(), Error> {
    create_dir_durable(path)?;
    let metadata = fs::metadata(path)?;
    ensure_corrupt(metadata.is_dir())?;
    ensure_corrupt(metadata.permissions().mode() & 0o077 == 0)
}

fn create_dir_durable(path: &Path) -> Result<(), Error> {
    reject_symlink(path)?;
    if path.exists() {
        return Ok(());
    }
    let parent = path.parent().ok_or(Error::Corrupt)?;
    create_dir_durable(parent)?;
    fs::DirBuilder::new().mode(0o700).create(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn read(path: &Path, maximum: usize) -> Result<Vec<u8>, Error> {
    reject_symlink(path)?;
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    ensure_corrupt(metadata.is_file())?;
    ensure_corrupt(metadata.len() <= maximum as u64)?;
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    ensure_corrupt(bytes.len() <= maximum)?;
    Ok(bytes)
}

fn read_optional(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, Error> {
    match read(path, maximum) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_once(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(existing) = read_optional(path, bytes.len())? {
        return ensure_corrupt(existing == bytes);
    }
    let temporary = path.with_extension("pending");
    reject_symlink(&temporary)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    File::open(path.parent().ok_or(Error::Corrupt)?)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
