//! Storage schema for the vaultprovider precompile.
//!
//! Slot layout (cumulative — `Set` occupies 2 slots: values-array head + the
//! 1-indexed positions mapping; `Map`/`Value` occupy 1):
//!
//! | slot | field                    | replaces (Solidity)                       |
//! |------|--------------------------|-------------------------------------------|
//! | 0    | `owner`                  | `OwnableUpgradeable` owner (genesis-seeded)|
//! | 1,2  | `assets`                 | `EnumerableSet _assets`                    |
//! | 3    | `asset_vaults`           | `mapping(asset => EnumerableSet vaults)`   |
//! | 4,5  | `liquidity_sources`      | `EnumerableSet _liquiditySources`          |
//! | 6    | `liquidity_source_types` | `mapping(account => LiquiditySource)`      |
//! | 7,8  | `liquidity_targets`      | `EnumerableSet _liquidityTargets`          |
//! | 9    | `liquidity_target_types` | `mapping(account => LiquidityTarget)`      |
//!
//! `asset_vaults` reserves slot 3 as the base of a Solidity-style
//! `mapping(address => EnumerableSet.AddressSet)`: the per-asset vault set lives
//! at the derived slot `keccak256(pad32(asset) ++ pad32(3))`, materialized on
//! demand by [`VaultProviderContract::asset_vault_set`].

use alloy_primitives::{Address, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::VAULT_PROVIDER_ADDRESS;
use outbe_primitives::storage::types::{StorageKey, StorageSet};

/// `LiquiditySource`/`LiquidityTarget` enum sentinel for "not set". Matches
/// `IVaultProvider.LiquiditySource.Unknown == 0` and
/// `IVaultProvider.LiquidityTarget.Unknown == 0`.
pub const UNKNOWN: u8 = 0;

/// EVM storage layout for the vaultprovider precompile.
#[storage_schema]
#[contract(addr = VAULT_PROVIDER_ADDRESS)]
pub struct VaultProviderContract {
    /// slot 0: owner (admin). Seeded at genesis; gates the `add*`/`remove*`
    /// management methods. Replaces `OwnableUpgradeable`.
    #[attribute(order = 0)]
    pub owner: outbe_primitives::storage::dsl::Value<Address>,

    /// slots 1–2: set of assets that have at least one registered vault.
    #[attribute(order = 1)]
    pub assets: outbe_primitives::storage::dsl::Set<Address>,

    /// slot 3: base slot of the per-asset vault sets. The value mapping is
    /// unused directly; `asset_vault_set(asset)` derives an enumerable
    /// `Set<Address>` at this mapping's per-key slot.
    #[attribute(order = 2)]
    pub asset_vaults: outbe_primitives::storage::dsl::Map<Address, U256>,

    /// slots 4–5: set of authorized liquidity-source accounts.
    #[attribute(order = 3)]
    pub liquidity_sources: outbe_primitives::storage::dsl::Set<Address>,

    /// slot 6: `account -> LiquiditySource` (stored as `u8`).
    #[attribute(order = 4)]
    pub liquidity_source_types: outbe_primitives::storage::dsl::Map<Address, u8>,

    /// slots 7–8: set of authorized liquidity-target accounts.
    #[attribute(order = 5)]
    pub liquidity_targets: outbe_primitives::storage::dsl::Set<Address>,

    /// slot 9: `account -> LiquidityTarget` (stored as `u8`).
    #[attribute(order = 6)]
    pub liquidity_target_types: outbe_primitives::storage::dsl::Map<Address, u8>,
}

impl<'storage> VaultProviderContract<'storage> {
    /// Returns the enumerable vault set for `asset`, laid out exactly as
    /// Solidity's `mapping(address => EnumerableSet.AddressSet)` — the set's
    /// base slot is the `asset_vaults` mapping's per-key slot.
    pub fn asset_vault_set(&self, asset: Address) -> StorageSet<'storage, Address> {
        let base = asset.mapping_slot(self.asset_vaults.base_slot());
        StorageSet::new(base, self.address, self.storage.clone())
    }

    /// First vault registered for `asset`, or `None` if the asset has no vault.
    pub fn first_vault(&self, asset: Address) -> outbe_primitives::error::Result<Option<Address>> {
        self.asset_vault_set(asset).at(0)
    }
}
