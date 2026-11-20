use crate::{errors::NodeHostRecordError, TransportError};
use rand::RngCore as _;
use std::{
    fs::{File, OpenOptions},
    io::{Read as _, Write as _},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::Path,
};
use zeroize::Zeroizing;

/// Persistent host identity bound by the canonical NodeHost authorization.
/// The private bytes are zeroized after deriving the public identity and never exposed.
#[derive(Clone)]
pub struct NodeHostIdentityKey {
    public: [u8; 32],
}

impl core::fmt::Debug for NodeHostIdentityKey {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("NodeHostIdentityKey")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

impl NodeHostIdentityKey {
    /// Derive a public identity from a deterministic test key.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn from_private(private: [u8; 32]) -> Self {
        Self::from_zeroizing(Zeroizing::new(private))
    }

    fn from_zeroizing(private: Zeroizing<[u8; 32]>) -> Self {
        let static_secret = x25519_dalek::StaticSecret::from(*private);
        let public = x25519_dalek::PublicKey::from(&static_secret).to_bytes();
        Self { public }
    }

    /// Create the NodeHost identity exactly once with owner-only permissions.
    /// Existing files reject. Callers must never rotate this key implicitly.
    pub fn create_new(path: &Path) -> Result<Self, TransportError> {
        let mut private = Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.fill_bytes(&mut *private);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(private.as_ref())?;
        file.sync_all()?;
        if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
            File::open(parent)?.sync_all()?;
        }
        Ok(Self::from_zeroizing(private))
    }

    /// Load the one existing NodeHost identity. Missing, truncated, symlinked or
    /// over-permissive/foreign-owned files fail closed. Loss never triggers key
    /// regeneration. `O_NOFOLLOW` and metadata from the opened descriptor avoid
    /// a path-check/read race.
    pub fn load(path: &Path) -> Result<Self, TransportError> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(NodeHostRecordError::IdentityKeyNotRegular.into());
        }
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(NodeHostRecordError::IdentityKeyMode.into());
        }
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(NodeHostRecordError::IdentityKeyOwner.into());
        }
        let mut private = Zeroizing::new([0u8; 32]);
        file.read_exact(&mut *private)?;
        let mut trailing = [0u8; 1];
        if file.read(&mut trailing)? != 0 {
            return Err(NodeHostRecordError::IdentityKeyLength.into());
        }
        Ok(Self::from_zeroizing(private))
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_host_key_store_is_write_once_owner_only_and_never_regenerates() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(super::super::NODE_HOST_IDENTITY_KEY_V1);
        let created = NodeHostIdentityKey::create_new(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            NodeHostIdentityKey::load(&path).unwrap().public(),
            created.public()
        );

        let create_error = NodeHostIdentityKey::create_new(&path).unwrap_err();
        assert!(matches!(
            create_error,
            TransportError::Io(ref error)
                if error.kind() == std::io::ErrorKind::AlreadyExists
        ));

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            NodeHostIdentityKey::load(&path),
            Err(TransportError::NodeHostRecord(
                NodeHostRecordError::IdentityKeyMode
            ))
        ));

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, [0x11; 32]).unwrap();
        assert_eq!(
            NodeHostIdentityKey::load(&path).unwrap().public(),
            NodeHostIdentityKey::from_private([0x11; 32]).public()
        );
        std::fs::write(&path, [0x11; 31]).unwrap();
        assert!(NodeHostIdentityKey::load(&path).is_err());
        std::fs::write(&path, [0x11; 33]).unwrap();
        assert!(matches!(
            NodeHostIdentityKey::load(&path),
            Err(TransportError::NodeHostRecord(
                NodeHostRecordError::IdentityKeyLength
            ))
        ));

        std::fs::remove_file(&path).unwrap();
        let target = root.path().join("target.key");
        std::fs::write(&target, [0x22; 32]).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(NodeHostIdentityKey::load(&path).is_err());
        std::fs::remove_file(&path).unwrap();

        assert!(matches!(
            NodeHostIdentityKey::load(&path),
            Err(TransportError::Io(ref error))
                if error.kind() == std::io::ErrorKind::NotFound
        ));
    }
}
