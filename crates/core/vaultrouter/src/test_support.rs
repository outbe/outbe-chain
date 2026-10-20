//! VaultRouter state fixtures for tests in other crates.

use alloy_primitives::Address;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::api::IVaultRouter::{StablesSource, StablesTarget};
use crate::VaultRouterContract;

/// Registers `vault` for `asset` and as the vault of reference currency `iso`.
pub fn register_vault(
    storage: &StorageHandle<'_>,
    asset: Address,
    vault: Address,
    iso: u16,
) -> Result<()> {
    let router = VaultRouterContract::new(storage.clone());
    router.assets.insert(asset)?;
    router.asset_vault_set(asset).insert(vault)?;
    router.reference_currency_vault_set(iso).insert(vault)?;
    router.vault_assets.write(&vault, asset)?;
    router.vault_reference_currencies.write(&vault, iso)
}

pub fn register_source(
    storage: &StorageHandle<'_>,
    source: Address,
    kind: StablesSource,
) -> Result<()> {
    let router = VaultRouterContract::new(storage.clone());
    router.liquidity_sources.insert(source)?;
    router.liquidity_source_types.write(&source, kind as u8)
}

pub fn register_target(
    storage: &StorageHandle<'_>,
    target: Address,
    kind: StablesTarget,
) -> Result<()> {
    let router = VaultRouterContract::new(storage.clone());
    router.liquidity_targets.insert(target)?;
    router.liquidity_target_types.write(&target, kind as u8)
}
