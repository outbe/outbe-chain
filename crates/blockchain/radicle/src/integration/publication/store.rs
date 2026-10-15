//! Durable immutable Review packages and one selected body per local attestation slot.

use super::{
    review::VerifiedReviewPublication, ObjectKind, PublicationManifest, MAX_MANIFEST_BYTES,
};
use crate::{
    integration::signing::{
        witness::{RepositoryWitnessBundle, MAX_WITNESS_BYTES},
        RepositorySigningMaterial,
    },
    manager::FinalizedPatchSigningState,
};
use alloy_primitives::B256;
use commonware_cryptography::{Hasher as _, Sha256};
use outbe_primitives::radicle_publication::{PublicationBody, PUBLICATION_BODY_LEN};
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
    publication: Arc<VerifiedReviewPublication>,
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

    /// Persist the complete verified package before making its body eligible for attestation.
    pub fn persist_and_select(
        &mut self,
        repo: &Repository,
        publication: Arc<VerifiedReviewPublication>,
        witness: &RepositoryWitnessBundle,
    ) -> Result<SelectedReviewPublication, Error> {
        let body_hash = publication.body().receipt_hash();
        let witness_bytes = witness.to_bytes()?;
        let witness_hash: B256 = Sha256::hash(&[&witness_bytes]).0.into();
        if witness_hash != publication.witness_hash() {
            return Err(Error::Corrupt);
        }
        let slot = review_slot_key(publication.body());
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
        write_once(&directory.join("body"), &publication.body().to_bytes())?;
        write_once(
            &directory.join("manifest"),
            &publication.manifest().to_bytes()?,
        )?;
        let odb = repo.raw().odb()?;
        for row in publication.manifest().objects() {
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
        Ok(SelectedReviewPublication { publication })
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
