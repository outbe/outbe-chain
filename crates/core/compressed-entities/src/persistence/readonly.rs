use super::{
    read_marker, read_required_tree_root, tables, validate_expected_environment_identity,
    EnvironmentIdentity, ExactParentIdentity, FinalizedMarker, MdbxSnapshot, PersistenceError,
    TreeNamespace, CE_SMT_RELATIVE_PATH, IDENTITY_KEY, LAST_APPLIED_KEY,
};
use crate::staging::FinalizedTreeSnapshot;
use reth_db::database::Database;
use reth_db::mdbx::tx::Tx;
use reth_db::mdbx::DatabaseArguments;
use reth_db::mdbx::RO;
use reth_db::open_db_read_only;
use reth_db::transaction::DbTx;
use reth_db::ClientVersion;
use reth_db::DatabaseEnv;
use std::path::Path;
use std::path::PathBuf;

/// Exporter-side view of the fixed CE environment.
///
/// This type is deliberately distinct from [`super::CeMdbx`]: it opens MDBX in
/// `Mode::ReadOnly`, exposes no mutation method and can only create an exact
/// authenticated finalized snapshot.
#[derive(Debug)]
pub struct CeMdbxReadOnly {
    path: PathBuf,
    identity: EnvironmentIdentity,
    pub(super) db: DatabaseEnv,
}

impl CeMdbxReadOnly {
    /// Opens the service-manager-configured CE environment without creating a
    /// directory, table or record.
    pub fn open(
        datadir: &Path,
        expected_identity: EnvironmentIdentity,
    ) -> Result<Self, PersistenceError> {
        validate_expected_environment_identity(&expected_identity)?;
        let path = datadir.join(CE_SMT_RELATIVE_PATH);
        let args = DatabaseArguments::new(ClientVersion::default());
        let db = open_db_read_only(&path, args).map_err(|error| PersistenceError::Database {
            path: path.clone(),
            message: format!("{error:#}"),
        })?;
        let store = Self {
            path,
            identity: expected_identity,
            db,
        };
        store.verify_existing()?;
        Ok(store)
    }

    #[must_use]
    pub const fn identity(&self) -> &EnvironmentIdentity {
        &self.identity
    }

    pub fn marker(&self) -> Result<FinalizedMarker, PersistenceError> {
        let tx = self.tx()?;
        let marker = read_marker(&tx, &self.path)?;
        tx.commit().map_err(|error| self.db_error(error))?;
        Ok(marker)
    }

    /// Opens one immutable transaction and rejects any marker/root mismatch
    /// before returning the view.
    pub fn open_exact(
        &self,
        required: ExactParentIdentity,
    ) -> Result<crate::staging::AuthenticatedCatalogView, PersistenceError> {
        let snapshot = self.open_snapshot()?;
        crate::staging::AuthenticatedCatalogView::open(snapshot, required)
            .map_err(|error| PersistenceError::Staging(error.to_string()))
    }

    fn open_snapshot(&self) -> Result<Box<dyn FinalizedTreeSnapshot>, PersistenceError> {
        let tx = self.tx()?;
        let marker = read_marker(&tx, &self.path)?;
        let catalog_root = read_required_tree_root(&tx, &self.path, TreeNamespace::Catalog)?;
        let wrapped = crate::sealed_root(catalog_root)
            .map_err(|error| PersistenceError::Staging(error.to_string()))?;
        if wrapped != marker.new_root {
            return Err(PersistenceError::CatalogWrapperMismatch {
                expected: marker.new_root,
                actual: wrapped,
            });
        }
        Ok(Box::new(MdbxSnapshot {
            path: self.path.clone(),
            tx,
            marker,
        }))
    }

    fn verify_existing(&self) -> Result<(), PersistenceError> {
        let tx = self.tx()?;
        let stored_identity = tx
            .get::<tables::CeMetadata>(IDENTITY_KEY.to_vec())
            .map_err(|error| self.db_error(error))?;
        let stored_marker = tx
            .get::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec())
            .map_err(|error| self.db_error(error))?;
        let (Some(identity), Some(marker)) = (stored_identity, stored_marker) else {
            return Err(PersistenceError::PartialEnvironmentInitialization);
        };
        let actual_identity = EnvironmentIdentity::decode(&identity)?;
        if actual_identity != self.identity {
            return Err(PersistenceError::EnvironmentIdentityMismatch {
                expected: self.identity.clone(),
                actual: actual_identity,
            });
        }
        let marker = FinalizedMarker::decode(&marker)?;
        if marker.commitment_scheme_version != self.identity.commitment_scheme_version {
            return Err(PersistenceError::EnvironmentMarkerSchemeMismatch);
        }
        let catalog_root = read_required_tree_root(&tx, &self.path, TreeNamespace::Catalog)?;
        let wrapped = crate::sealed_root(catalog_root)
            .map_err(|error| PersistenceError::Staging(error.to_string()))?;
        if wrapped != marker.new_root {
            return Err(PersistenceError::CatalogWrapperMismatch {
                expected: marker.new_root,
                actual: wrapped,
            });
        }
        tx.commit().map_err(|error| self.db_error(error))
    }

    fn tx(&self) -> Result<Tx<RO>, PersistenceError> {
        self.db.tx().map_err(|error| self.db_error(error))
    }

    fn db_error(&self, error: impl std::fmt::Display) -> PersistenceError {
        PersistenceError::Database {
            path: self.path.clone(),
            message: error.to_string(),
        }
    }
}
