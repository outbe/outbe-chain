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

impl SelectedReviewPublication {
    pub fn body(&self) -> &PublicationBody {
        match &self.publication {
            SelectedPatch::Review(p) => p.body(),
            SelectedPatch::Merge(p) => p.body(),
            SelectedPatch::Material(p) => p.body(),
        }
    }
    pub fn manifest(&self) -> &PublicationManifest {
        match &self.publication {
            SelectedPatch::Review(p) => p.manifest(),
            SelectedPatch::Merge(p) => p.manifest(),
            SelectedPatch::Material(p) => p.manifest(),
        }
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
        if receipt.receipt_hash != receipt.body.receipt_hash() {
            return Err(Error::Corrupt);
        }
        manifest.bind_receipt(&receipt.body)?;
        let directory = self.package_dir(receipt.receipt_hash);
        private_dir(&directory)?;
        write_once(&directory.join("body"), &receipt.body.to_bytes())?;
        write_once(&directory.join("manifest"), &manifest.to_bytes()?)
    }

    pub fn load_checkpoint_manifest(
        &self,
        checkpoint: &FinalizedPublicationReceipt,
    ) -> Result<PublicationManifest, Error> {
        let receipt = checkpoint.receipt();
        let directory = self.package_dir(receipt.receipt_hash);
        reject_symlink(&directory)?;
        if receipt.receipt_hash != receipt.body.receipt_hash()
            || read(&directory.join("body"), PUBLICATION_BODY_LEN)? != receipt.body.to_bytes()
        {
            return Err(Error::Corrupt);
        }
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
        if sequence == 0 {
            return Err(Error::Corrupt);
        }
        let mut snapshot: Option<CheckpointOwnerSnapshot> = None;
        for expected in 1..=sequence {
            let checkpoint = reader
                .read_current_publication_receipt(expected)?
                .ok_or(Error::MissingCheckpoint(expected))?;
            if checkpoint.receipt().body.sequence != expected
                || (expected == sequence && checkpoint.receipt() != target.receipt())
            {
                return Err(Error::Corrupt);
            }
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
        self.persist(
            repo,
            publication.body(),
            publication.manifest(),
            publication.witness_hash(),
            witness,
            None,
        )?;
        Ok(SelectedReviewPublication {
            publication: SelectedPatch::Review(publication),
        })
    }

    pub fn persist_and_select_merge(
        &mut self,
        repo: &Repository,
        publication: Arc<super::merge::VerifiedMergePublication>,
        witness: &RepositoryWitnessBundle,
    ) -> Result<SelectedReviewPublication, Error> {
        self.persist(
            repo,
            publication.body(),
            publication.manifest(),
            publication.witness_hash(),
            witness,
            None,
        )?;
        Ok(SelectedReviewPublication {
            publication: SelectedPatch::Merge(publication),
        })
    }

    pub fn persist_and_select_bootstrap(
        &mut self,
        repo: &Repository,
        publication: Arc<crate::integration::bootstrap::VerifiedBootstrapPublication>,
        witness: &RepositoryWitnessBundle,
    ) -> Result<SelectedBootstrapPublication, Error> {
        self.persist(
            repo,
            publication.body(),
            publication.manifest(),
            publication.witness_hash(),
            witness,
            Some((publication.descriptor(), publication.content_manifest())),
        )?;
        Ok(SelectedBootstrapPublication { publication })
    }

    pub fn persist_and_select_material(
        &mut self,
        repo: &Repository,
        publication: Arc<super::binding::VerifiedMaterialPublication>,
        witness: &RepositoryWitnessBundle,
    ) -> Result<SelectedReviewPublication, Error> {
        self.persist(
            repo,
            publication.body(),
            publication.manifest(),
            publication.witness_hash(),
            witness,
            None,
        )?;
        Ok(SelectedReviewPublication {
            publication: SelectedPatch::Material(publication),
        })
    }

    fn persist(
        &mut self,
        repo: &Repository,
        body: &PublicationBody,
        manifest: &PublicationManifest,
        expected_witness_hash: B256,
        witness: &RepositoryWitnessBundle,
        bootstrap: Option<(&GovernmentsDescriptor, &[u8])>,
    ) -> Result<(), Error> {
        let body_hash = body.receipt_hash();
        let witness_bytes = witness.to_bytes()?;
        let witness_hash: B256 = Sha256::hash(&[&witness_bytes]).0.into();
        if witness_hash != expected_witness_hash {
            return Err(Error::Corrupt);
        }
        let slot = review_slot_key(body);
        if let Some((body, witness)) = self.selection(slot)? {
            if body != body_hash {
                return Err(Error::SlotConflict);
            }
            // The durable pointer keeps its original witness even if a new valid subset arrives.
            self.load(body, witness, repo)?;
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
            if size > MAX_OBJECT_BYTES || kind != object_type(row.kind) {
                return Err(Error::Corrupt);
            }
            let object = odb.read(oid.into())?;
            if object.kind() != object_type(row.kind)
                || object.data().len() > MAX_OBJECT_BYTES
                || hex::encode(Sha256::hash(&[object.data()]).0) != row.sha256
                || git::raw::Oid::hash_object(object.kind(), object.data())? != oid.into()
            {
                return Err(Error::Corrupt);
            }
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

    /// Rebuild read grants through the requested accepted checkpoint from own receipts/q-proofs.
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
        if repo.raw().path().canonicalize()? != path.canonicalize()? {
            return Err(Error::Corrupt);
        }
        let package = self
            .load_selected(review_slot_key(body), &repo)?
            .ok_or(Error::Corrupt)?;
        if package.body != *body {
            return Err(Error::Corrupt);
        }
        let (descriptor, content) = self.load_bootstrap_inputs(&checkpoint)?;
        let verifier = super::super::signing::accepted::AcceptedPublicationVerifier::from_receipt(
            checkpoint.clone(),
            package.manifest.clone(),
            package.witness.public_output(),
        )?;
        let proof = crate::integration::bootstrap::verify_accepted_bootstrap(
            &repo,
            &descriptor,
            &content,
            &verifier,
            &package.witness,
        )
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
            let checkpoint = reader
                .read_current_publication_receipt(sequence)?
                .ok_or(Error::MissingCheckpoint(sequence))?;
            let package = self
                .load_selected(review_slot_key(&checkpoint.receipt().body), &repo)?
                .ok_or(Error::Corrupt)?;
            if package.body != checkpoint.receipt().body || package.body.sequence != sequence {
                return Err(Error::Corrupt);
            }
            let verifier =
                super::super::signing::accepted::AcceptedPublicationVerifier::from_receipt(
                    checkpoint.clone(),
                    package.manifest.clone(),
                    package.witness.public_output(),
                )?;
            match package.body.action {
                PublicationAction::Review => {
                    let approval = reader
                        .read_current_patch_approval(package.body.proposal_id)?
                        .ok_or(Error::Corrupt)?;
                    let proof = super::review::verify_accepted_review_publication(
                        &repo,
                        &approval,
                        &snapshot,
                        &verifier,
                        &package.witness,
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
                        &repo,
                        package.body.proposal_id,
                        snapshot.checkpoint(),
                    )?;
                    let proof = super::merge::verify_accepted_merge_publication(
                        &repo,
                        &approval,
                        &snapshot,
                        &review,
                        &verifier,
                        &package.witness,
                    )
                    .map_err(|_| Error::Corrupt)?;
                    policy.accept_merge(&proof).map_err(|_| Error::Corrupt)?;
                }
                PublicationAction::BindMaterial => {
                    let proof = super::binding::verify_accepted_material_publication(
                        &snapshot,
                        &verifier,
                        &package.witness,
                    )
                    .map_err(|_| Error::Corrupt)?;
                    policy.accept_material(&proof).map_err(|_| Error::Corrupt)?;
                }
                _ => return Err(Error::Corrupt),
            }
            snapshot.advance(checkpoint, &package.manifest)?;
        }
        if snapshot.checkpoint().receipt() != target.receipt() {
            return Err(Error::Corrupt);
        }
        Ok(repo)
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
            let package = self
                .load_selected(review_slot_key(&receipt.receipt().body), repo)?
                .ok_or(Error::Corrupt)?;
            if package.body != receipt.receipt().body {
                return Err(Error::Corrupt);
            }
            let approval = reader
                .read_current_patch_approval(proposal)?
                .ok_or(Error::Corrupt)?;
            let verifier =
                super::super::signing::accepted::AcceptedPublicationVerifier::from_receipt(
                    receipt,
                    package.manifest,
                    package.witness.public_output(),
                )?;
            return super::review::verify_accepted_review_publication(
                repo,
                &approval,
                &snapshot,
                &verifier,
                &package.witness,
            )
            .map_err(|_| Error::Corrupt);
        }
        Err(Error::Corrupt)
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
        if read(
            &directory.join("ready").join(hex::encode(witness_hash)),
            READY.len(),
        )? != READY
        {
            return Err(Error::Corrupt);
        }
        let body =
            PublicationBody::from_bytes(&read(&directory.join("body"), PUBLICATION_BODY_LEN)?)
                .map_err(|_| Error::Corrupt)?;
        if body.receipt_hash() != body_hash {
            return Err(Error::Corrupt);
        }
        let manifest = PublicationManifest::from_bytes(&read(
            &directory.join("manifest"),
            MAX_MANIFEST_BYTES,
        )?)?;
        if manifest.hash()? != body.package_manifest_hash {
            return Err(Error::Corrupt);
        }
        let bytes = read(
            &directory.join("witnesses").join(hex::encode(witness_hash)),
            MAX_WITNESS_BYTES,
        )?;
        if B256::from(Sha256::hash(&[&bytes]).0) != witness_hash {
            return Err(Error::Corrupt);
        }
        let witness = RepositoryWitnessBundle::from_bytes(&bytes)?;
        let odb = repo.raw().odb()?;
        for row in manifest.objects() {
            let bytes = read(
                &directory.join("objects").join(&row.sha256),
                MAX_OBJECT_BYTES,
            )?;
            let oid: git::Oid = row.oid.parse().map_err(|_| Error::Corrupt)?;
            let kind = object_type(row.kind);
            if hex::encode(Sha256::hash(&[&bytes]).0) != row.sha256
                || git::raw::Oid::hash_object(kind, &bytes)? != oid.into()
            {
                return Err(Error::Corrupt);
            }
            if odb.write(kind, &bytes)? != oid.into() {
                return Err(Error::Corrupt);
            }
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
        let bytes = match read(&path, 68) {
            Ok(bytes) => bytes,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        if bytes.len() != 68 || &bytes[..4] != SLOT {
            return Err(Error::Corrupt);
        }
        Ok(Some((
            B256::from_slice(&bytes[4..36]),
            B256::from_slice(&bytes[36..]),
        )))
    }

    fn select(&mut self, slot: B256, body: B256, witness: B256) -> Result<(), Error> {
        if let Some((selected, _)) = self.selection(slot)? {
            return if selected == body {
                Ok(())
            } else {
                Err(Error::SlotConflict)
            };
        }
        write_once(
            &self.root.join("slots").join(hex::encode(slot)),
            &[SLOT, body.as_slice(), witness.as_slice()].concat(),
        )
    }
}

fn validate_bootstrap_inputs(
    body: &PublicationBody,
    descriptor: &GovernmentsDescriptor,
    content: &[u8],
) -> Result<(), Error> {
    if body.action != PublicationAction::Bootstrap
        || body.sequence != 1
        || !body.previous_receipt_hash.is_zero()
        || descriptor.chain_id != body.chain_id
        || descriptor.hash().map_err(|_| Error::Corrupt)? != body.descriptor_hash
        || content.len() > MAX_MANIFEST_BYTES
        || B256::from(Sha256::hash(&[content]).0) != descriptor.initial_content_manifest_sha256
    {
        return Err(Error::Corrupt);
    }
    Ok(())
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
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Error::Corrupt),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn private_dir(path: &Path) -> Result<(), Error> {
    reject_symlink(path)?;
    if !path.exists() {
        create_dir_durable(path)?;
    }
    let metadata = fs::metadata(path)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::Corrupt);
    }
    Ok(())
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
    if !file.metadata()?.is_file() || file.metadata()?.len() > maximum as u64 {
        return Err(Error::Corrupt);
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(Error::Corrupt);
    }
    Ok(bytes)
}

fn write_once(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    match read(path, bytes.len()) {
        Ok(existing) => {
            return if existing == bytes {
                Ok(())
            } else {
                Err(Error::Corrupt)
            }
        }
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
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
mod tests {
    use super::*;
    use crate::manager::{FinalizedBlock, ManagerError};
    use outbe_primitives::radicle_publication::PublicationAction;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct ReceiptSource {
        rows: BTreeMap<u64, FinalizedPublicationReceipt>,
        reads: Mutex<Vec<u64>>,
        unavailable: bool,
    }

    impl RepositoryReceiptReader for ReceiptSource {
        fn read_current_publication_receipt(
            &self,
            sequence: u64,
        ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError> {
            self.reads.lock().unwrap().push(sequence);
            if self.unavailable {
                Err(ManagerError::Finality(
                    "local finalized state unavailable".into(),
                ))
            } else {
                Ok(self.rows.get(&sequence).cloned())
            }
        }
    }

    // Canonical manifest/receipt commitments, not a complete native Git package or chain fixture.
    fn checkpoint_chain() -> Vec<(FinalizedPublicationReceipt, PublicationManifest)> {
        let (mut body, initial) = super::super::tests::bootstrap_fixture();
        let mut rows = vec![];
        for sequence in 1..=4 {
            let manifest = if sequence == 1 {
                initial.clone()
            } else {
                body.action = PublicationAction::BindMaterial;
                body.epoch += 1;
                body.material_version += 1;
                body.material_hash = B256::repeat_byte(sequence as u8);
                PublicationManifest::new(vec![], vec![], vec![], initial.owner_snapshot().to_vec())
                    .unwrap()
            };
            body.sequence = sequence;
            body.package_manifest_hash = manifest.hash().unwrap();
            let checkpoint = FinalizedPublicationReceipt {
                block: FinalizedBlock {
                    number: 20,
                    hash: B256::repeat_byte(20),
                },
                receipt: outbe_radicleregistry::publication::PublicationReceipt {
                    body: body.clone(),
                    accepted_height: sequence,
                    receipt_hash: body.receipt_hash(),
                },
            };
            body.previous_receipt_hash = checkpoint.receipt().receipt_hash;
            rows.push((checkpoint, manifest));
        }
        rows
    }

    fn receipt_source(
        rows: &[(FinalizedPublicationReceipt, PublicationManifest)],
    ) -> ReceiptSource {
        ReceiptSource {
            rows: rows
                .iter()
                .map(|(r, _)| (r.receipt().body.sequence, r.clone()))
                .collect(),
            reads: Mutex::new(vec![]),
            unavailable: false,
        }
    }

    #[test]
    fn accepted_manifest_cache_replays_exact_snapshot_after_reopen_without_selecting_package() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("archive");
        let rows = checkpoint_chain();
        let mut store = ReviewPackageStore::open(&root).unwrap();
        for index in [3, 1, 0, 2, 0] {
            store
                .persist_checkpoint_manifest(&rows[index].0, &rows[index].1)
                .unwrap();
        }
        let target = &rows[3].0;
        assert!(store
            .selection(review_slot_key(&target.receipt().body))
            .unwrap()
            .is_none());
        assert!(!store
            .package_dir(target.receipt().receipt_hash)
            .join("ready")
            .exists());
        let expected = rows[1..].iter().fold(
            CheckpointOwnerSnapshot::bootstrap(rows[0].0.clone(), &rows[0].1).unwrap(),
            |mut snapshot, (receipt, manifest)| {
                snapshot.advance(receipt.clone(), manifest).unwrap();
                snapshot
            },
        );
        drop(store);
        let reopened = ReviewPackageStore::open(&root).unwrap();
        let source = receipt_source(&rows);
        let actual = reopened
            .replay_checkpoint_snapshot(&source, target)
            .unwrap();
        assert_eq!(actual.checkpoint(), expected.checkpoint());
        assert_eq!(actual.owner_refs(), expected.owner_refs());
        assert_eq!(actual.sigrefs(), expected.sigrefs());
        assert_eq!(*source.reads.lock().unwrap(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn checkpoint_replay_requires_trusted_receipts_and_rejects_gaps_forks_or_foreign_target() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = ReviewPackageStore::open(directory.path().join("archive")).unwrap();
        let rows = checkpoint_chain();
        for (receipt, manifest) in &rows {
            store
                .persist_checkpoint_manifest(receipt, manifest)
                .unwrap();
        }
        let target = &rows[3].0;
        let mut source = receipt_source(&rows);
        source.unavailable = true;
        assert!(matches!(
            store.replay_checkpoint_snapshot(&source, target),
            Err(Error::Authority(_))
        ));
        source.unavailable = false;
        let missing = source.rows.remove(&2).unwrap();
        assert!(matches!(
            store.replay_checkpoint_snapshot(&source, target),
            Err(Error::MissingCheckpoint(2))
        ));
        source.rows.insert(2, rows[2].0.clone());
        assert!(matches!(
            store.replay_checkpoint_snapshot(&source, target),
            Err(Error::Corrupt)
        ));
        let mut fork = missing;
        fork.receipt.body.previous_receipt_hash = B256::repeat_byte(99);
        fork.receipt.receipt_hash = fork.receipt.body.receipt_hash();
        store
            .persist_checkpoint_manifest(&fork, &rows[1].1)
            .unwrap();
        source.rows.insert(2, fork);
        assert!(store.replay_checkpoint_snapshot(&source, target).is_err());
        source = receipt_source(&rows);
        let mut wrong_target = target.clone();
        wrong_target.receipt.body.genesis_hash = B256::repeat_byte(99);
        wrong_target.receipt.receipt_hash = wrong_target.receipt.body.receipt_hash();
        assert!(store
            .replay_checkpoint_snapshot(&source, &wrong_target)
            .is_err());
        wrong_target.receipt.body.sequence = 0;
        assert!(store
            .replay_checkpoint_snapshot(&source, &wrong_target)
            .is_err());
    }

    #[test]
    fn checkpoint_manifest_cache_rejects_corruption_missing_files_and_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = ReviewPackageStore::open(directory.path().join("archive")).unwrap();
        let rows = checkpoint_chain();
        for (receipt, manifest) in &rows {
            store
                .persist_checkpoint_manifest(receipt, manifest)
                .unwrap();
        }
        let checkpoint = &rows[1].0;
        let package = store.package_dir(checkpoint.receipt().receipt_hash);
        let manifest_path = package.join("manifest");
        let bytes = rows[1].1.to_bytes().unwrap();
        fs::remove_file(&manifest_path).unwrap();
        assert!(store
            .replay_checkpoint_snapshot(&receipt_source(&rows), &rows[3].0)
            .is_err());
        fs::write(&manifest_path, vec![0; MAX_MANIFEST_BYTES + 1]).unwrap();
        assert!(matches!(
            store.load_checkpoint_manifest(checkpoint),
            Err(Error::Corrupt)
        ));
        fs::write(&manifest_path, &bytes).unwrap();
        fs::write(package.join("body"), b"corrupt").unwrap();
        assert!(matches!(
            store.load_checkpoint_manifest(checkpoint),
            Err(Error::Corrupt)
        ));
        fs::write(package.join("body"), checkpoint.receipt().body.to_bytes()).unwrap();
        let mut wrong = rows[1].1.clone();
        wrong.owner_snapshot[0].oid = "99".repeat(20);
        fs::write(&manifest_path, wrong.to_bytes().unwrap()).unwrap();
        assert!(store.load_checkpoint_manifest(checkpoint).is_err());
        fs::write(&manifest_path, b"{}").unwrap();
        assert!(store.load_checkpoint_manifest(checkpoint).is_err());
        fs::remove_file(&manifest_path).unwrap();
        let outside = directory.path().join("untouched");
        fs::write(&outside, &bytes).unwrap();
        std::os::unix::fs::symlink(&outside, &manifest_path).unwrap();
        assert!(matches!(
            store.load_checkpoint_manifest(checkpoint),
            Err(Error::Corrupt)
        ));
        assert_eq!(fs::read(&outside).unwrap(), bytes);
        assert!(store
            .persist_checkpoint_manifest(checkpoint, &rows[0].1)
            .is_err());
        assert_eq!(fs::read(&outside).unwrap(), bytes);
    }

    #[test]
    fn selected_slot_survives_reopen_and_cannot_choose_another_body() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("nested/store");
        let mut store = ReviewPackageStore::open(&root).unwrap();
        assert!(matches!(
            ReviewPackageStore::open(&root),
            Err(Error::Ownership(_))
        ));
        let slot = B256::repeat_byte(1);
        let body = B256::repeat_byte(2);
        let witness = B256::repeat_byte(3);
        store.select(slot, body, witness).unwrap();
        drop(store);
        let mut reopened = ReviewPackageStore::open(&root).unwrap();
        assert_eq!(reopened.selection(slot).unwrap(), Some((body, witness)));
        reopened.select(slot, body, B256::repeat_byte(4)).unwrap();
        assert!(matches!(
            reopened.select(slot, B256::repeat_byte(5), witness),
            Err(Error::SlotConflict)
        ));
        assert_eq!(reopened.selection(slot).unwrap(), Some((body, witness)));
        fs::write(root.join("slots").join(hex::encode(slot)), b"corrupt").unwrap();
        assert!(matches!(reopened.selection(slot), Err(Error::Corrupt)));
    }

    #[test]
    fn atomic_artifacts_recover_uncommitted_temporary_bytes_without_overwriting_committed_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("artifact");
        fs::write(path.with_extension("pending"), b"partial").unwrap();
        write_once(&path, b"complete").unwrap();
        assert_eq!(read(&path, 8).unwrap(), b"complete");
        write_once(&path, b"complete").unwrap();
        assert!(matches!(
            write_once(&path, b"different"),
            Err(Error::Corrupt)
        ));
        assert_eq!(read(&path, 8).unwrap(), b"complete");
        assert!(matches!(read(&path, 7), Err(Error::Corrupt)));
        assert!(!path.with_extension("pending").exists());
    }

    #[test]
    fn symlinks_and_public_store_directories_fail_without_changing_the_target() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::write(&target, b"preserve").unwrap();
        let path = directory.path().join("artifact");
        std::os::unix::fs::symlink(&target, path.with_extension("pending")).unwrap();
        assert!(matches!(write_once(&path, b"replace"), Err(Error::Corrupt)));
        assert_eq!(fs::read(&target).unwrap(), b"preserve");
        let public = directory.path().join("public");
        fs::create_dir(&public).unwrap();
        fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            ReviewPackageStore::open(&public),
            Err(Error::Corrupt)
        ));
        assert_eq!(
            fs::metadata(&public).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
