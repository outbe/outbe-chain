use super::{
    read_required_tree_root, tables, validate_root, CeMdbx, FinalizedMarker, PersistenceError,
    TreeNamespace, LAST_APPLIED_KEY,
};
use reth_db::database::Database;
use reth_db::transaction::DbTx;
use reth_db::transaction::DbTxMut;

impl CeMdbx {
    /// Seeds an exact finalized parent for cross-crate executor tests without
    /// replaying every empty intermediate block. This is unavailable in
    /// production builds and preserves the persisted catalog/root invariant.
    #[cfg(feature = "test-utils")]
    #[doc(hidden)]
    pub fn test_seed_finalized_marker(
        &self,
        marker: FinalizedMarker,
    ) -> Result<(), PersistenceError> {
        if marker.commitment_scheme_version != self.identity.commitment_scheme_version {
            return Err(PersistenceError::EnvironmentMarkerSchemeMismatch);
        }
        validate_root(marker.parent_root)?;
        validate_root(marker.new_root)?;
        let tx = self.db.tx_mut().map_err(|error| self.db_error(error))?;
        let catalog_root = read_required_tree_root(&tx, &self.path, TreeNamespace::Catalog)?;
        let wrapped = crate::sealed_root(catalog_root)
            .map_err(|error| PersistenceError::Staging(error.to_string()))?;
        if wrapped != marker.new_root {
            return Err(PersistenceError::CatalogWrapperMismatch {
                expected: marker.new_root,
                actual: wrapped,
            });
        }
        tx.put::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec(), marker.encode().to_vec())
            .map_err(|error| self.db_error(error))?;
        tx.commit()
            .map_err(|error| PersistenceError::CommitOutcomeUnknown {
                path: self.path.clone(),
                marker,
                message: error.to_string(),
            })
    }
}
