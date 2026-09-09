//! Local custody trust-anchor storage.
use crate::{errors::VaultRouterError, schema::VaultRouterContract};
use alloy_primitives::Address;
use outbe_primitives::{error::Result, storage::StorageHandle};

pub(crate) fn bind_bundle_custody(storage: &StorageHandle<'_>, custody: Address) -> Result<()> {
    let contract = VaultRouterContract::new(storage.clone());
    if !contract.bundle_custody.read()?.is_zero() {
        return Err(VaultRouterError::BundleCustodyAlreadyConfigured.into());
    }
    contract.bundle_custody.write(custody)
}
