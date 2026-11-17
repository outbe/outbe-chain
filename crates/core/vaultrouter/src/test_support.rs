//! VaultRouter state fixtures for tests in other crates.

use alloy_primitives::{Address, Bytes};
use alloy_sol_types::SolCall;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_system_common::addresses::VAULT_ROUTER_ADDRESS;

use crate::errors::VaultRouterError;
use crate::precompile::IVaultRouter::{self, StablecoinSource, StablecoinTarget};
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
    kind: StablecoinSource,
) -> Result<()> {
    let router = VaultRouterContract::new(storage.clone());
    router.stablecoin_sources.insert(source)?;
    router.stablecoin_source_types.write(&source, kind as u8)
}

pub fn register_target(
    storage: &StorageHandle<'_>,
    target: Address,
    kind: StablecoinTarget,
) -> Result<()> {
    let router = VaultRouterContract::new(storage.clone());
    router.stablecoin_targets.insert(target)?;
    router.stablecoin_target_types.write(&target, kind as u8)
}

/// Answers the router's `depositFrom` with no return data in a storage-only harness, where
/// the router does not run. A settlement payment that reaches the router then fails with
/// [`unanswered_deposit_from`], and its checkpoint rolls back.
pub fn stub_unanswered_deposit_from(provider: &mut HashMapStorageProvider) {
    provider.stub_sub_call_at_selector(
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::depositFromCall::SELECTOR,
        Bytes::new(),
    );
}

/// The error of a settlement payment that reached the stub of [`stub_unanswered_deposit_from`].
pub fn unanswered_deposit_from() -> PrecompileError {
    VaultRouterError::RouterCallUndecodable("depositFrom").into()
}

/// The last `depositFrom` call that the router received in a storage-only harness.
pub fn last_deposit_from(
    provider: &HashMapStorageProvider,
) -> Option<IVaultRouter::depositFromCall> {
    provider
        .sub_call_inputs()
        .iter()
        .rev()
        .filter(|input| input.target == VAULT_ROUTER_ADDRESS)
        .find_map(|input| IVaultRouter::depositFromCall::abi_decode(&input.calldata).ok())
}
