use alloy_primitives::{Address, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::storage::types::{StorageKey, StorageSet};

/// `StablesSource`/`StablesTarget` enum sentinel for "not set". Matches
/// `IVaultRouter.StablesSource.Unknown == 0` and
/// `IVaultRouter.StablesTarget.Unknown == 0`.
pub const UNKNOWN: u8 = 0;

/// One CCA-held liquidity reservation. A zero `asset` means the slot is empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[storage_record(exists_field = asset)]
pub struct LiquidityReservation {
    #[key]
    pub id: U256,
    #[attribute(order = 0)]
    pub asset: Address,
    #[attribute(order = 1)]
    pub amount_minor: U256,
    #[attribute(order = 2)]
    pub credis_owner: Address,
    #[attribute(order = 3)]
    pub cca: Address,
    #[attribute(order = 4)]
    pub vault: Address,
    #[attribute(order = 5)]
    pub expires_at: u64,
    #[attribute(order = 6)]
    pub gratis_minor: U256,
    #[attribute(order = 7)]
    pub snapshot_id: U256,
    #[attribute(order = 8)]
    pub entry_price_minor: U256,
    #[attribute(order = 9)]
    pub valuation_price_minor: U256,
    #[attribute(order = 10)]
    pub policy_rate: U256,
    #[attribute(order = 11)]
    pub issuance_currency: u16,
    #[attribute(order = 12)]
    pub asset_decimals: u8,
    #[attribute(order = 13)]
    pub reference_currency: u16,
    #[attribute(order = 14)]
    pub call_anchor_price_minor: U256,
    #[attribute(order = 15)]
    pub source: Address,
}

/// EVM storage layout for the vaultrouter precompile.
#[storage_schema]
#[contract(addr = VAULT_ROUTER_ADDRESS)]
pub struct VaultRouterContract {
    /// slot 0: owner (admin). Seeded at genesis. Gates the `add*`/`remove*`
    /// management methods. Replaces `OwnableUpgradeable`.
    #[attribute(order = 0)]
    pub owner: outbe_primitives::storage::dsl::Value<Address>,

    /// slots 1-2: set of assets that have at least one registered vault.
    #[attribute(order = 1)]
    pub assets: outbe_primitives::storage::dsl::Set<Address>,

    /// slot 3: base slot of the per-asset vault sets. No code uses the value
    /// mapping directly. `asset_vault_set(asset)` derives an enumerable
    /// `Set<Address>` at this mapping's per-key slot.
    #[attribute(order = 2)]
    pub asset_vaults: outbe_primitives::storage::dsl::Map<Address, U256>,

    /// slots 4-5: set of authorized liquidity-source accounts.
    #[attribute(order = 3)]
    pub liquidity_sources: outbe_primitives::storage::dsl::Set<Address>,

    /// slot 6: `account -> StablesSource` (stored as `u8`).
    #[attribute(order = 4)]
    pub liquidity_source_types: outbe_primitives::storage::dsl::Map<Address, u8>,

    /// slots 7-8: set of authorized liquidity-target accounts.
    #[attribute(order = 5)]
    pub liquidity_targets: outbe_primitives::storage::dsl::Set<Address>,

    /// slot 9: `account -> StablesTarget` (stored as `u8`).
    #[attribute(order = 6)]
    pub liquidity_target_types: outbe_primitives::storage::dsl::Map<Address, u8>,

    /// slot 10: base slot of the per-ISO-4217 vault sets.
    #[attribute(order = 7)]
    pub reference_currency_vaults: outbe_primitives::storage::dsl::Map<u16, U256>,

    /// slot 11: vault -> ISO-4217 code captured at registration time.
    #[attribute(order = 8)]
    pub vault_reference_currencies: outbe_primitives::storage::dsl::Map<Address, u16>,

    /// slot 12: monotonic nonce used directly as the reservation id.
    #[attribute(order = 9)]
    pub reservation_nonce: outbe_primitives::storage::dsl::Value<U256>,

    /// slot 13: CCA stables reservations keyed by id.
    #[attribute(order = 10)]
    pub reservations: outbe_primitives::storage::dsl::Map<U256, LiquidityReservation>,
}

impl<'storage> VaultRouterContract<'storage> {
    /// Returns the enumerable vault set for `asset`, laid out exactly as
    /// Solidity's `mapping(address => EnumerableSet.AddressSet)`. The set's
    /// base slot is the `asset_vaults` mapping's per-key slot.
    pub fn asset_vault_set(&self, asset: Address) -> StorageSet<'storage, Address> {
        let base = asset.mapping_slot(self.asset_vaults.base_slot());
        StorageSet::new(base, self.address, self.storage.clone())
    }

    /// Returns the enumerable vault set for `iso_code`.
    pub fn reference_currency_vault_set(&self, iso_code: u16) -> StorageSet<'storage, Address> {
        let base = iso_code.mapping_slot(self.reference_currency_vaults.base_slot());
        StorageSet::new(base, self.address, self.storage.clone())
    }

    /// First vault registered for `asset`, or `None` if the asset has no vault.
    pub fn first_vault(&self, asset: Address) -> outbe_primitives::error::Result<Option<Address>> {
        self.asset_vault_set(asset).at(0)
    }
}

impl From<LiquidityReservation> for crate::api::IVaultRouter::LiquidityReservation {
    fn from(r: LiquidityReservation) -> Self {
        Self {
            asset: r.asset,
            amountMinor: r.amount_minor,
            credisOwner: r.credis_owner,
            cca: r.cca,
            vault: r.vault,
            expiresAt: r.expires_at,
            gratisMinor: r.gratis_minor,
            snapshotId: r.snapshot_id,
            entryPriceMinor: r.entry_price_minor,
            valuationPriceMinor: r.valuation_price_minor,
            policyRate: r.policy_rate,
            issuanceCurrency: r.issuance_currency,
            assetDecimals: r.asset_decimals,
            referenceCurrency: r.reference_currency,
            callAnchorPriceMinor: r.call_anchor_price_minor,
            source: r.source,
        }
    }
}
