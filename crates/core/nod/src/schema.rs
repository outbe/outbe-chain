use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::NOD_ADDRESS;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use serde::{Deserialize, Serialize};

/// Read-time lifecycle state. Values are part of the public Nod ABI, not storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EffectiveState {
    Issued = 0,
    Qualified = 1,
    Called = 2,
    Settled = 3,
    Forfeited = 4,
}

/// Plain calculation inputs. Production issuance stores an authenticated encrypted NOD.
/// The identity is derived from owner and day; settlement uses the entry price and load.
/// `issued_at` comes from the block rather than these inputs.
#[derive(Debug, Clone, PartialEq)]
pub struct NodIssueParams {
    pub owner: Address,
    pub worldwide_day: WorldwideDay,
    pub league_id: u16,
    pub gratis_load_minor: U256,
    pub entry_price_minor: U256,
    pub issuance_currency: u16,
    /// Reference currency (ISO 4217 numeric) propagated from the originating
    /// Tribute. Used for off-chain pricing references on the Nod.
    pub reference_currency: u16,
}

/// Runtime view of one self-contained encrypted canonical NOD body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodItemState {
    pub nod_id: WwdEntityId,

    pub owner: Address,

    pub encrypted: outbe_primitives::nod_encryption::EncryptedNodV2,

    pub worldwide_day: WorldwideDay,

    pub league_id: u16,

    pub bucket_key: B256,

    pub issuance_currency: u16,

    pub reference_currency: u16,

    pub issued_at: u64,

    #[serde(default)]
    pub is_settled: bool,
}

/// Call terms a bucket seals at issuance, and the only terms every later check
/// reads. Grouped rather than passed positionally so five same-typed numbers
/// cannot silently swap places on the way to storage.
///
/// Second-encoded like `GemData`'s window, threshold and notice. The daily scan
/// divides them back into day counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallTerms {
    /// `entry_price_minor x (100 + call_rate) / 100`.
    pub call_price_minor: U256,
    /// Selects the `COEN/<iso>` VWAP series the breach is measured on.
    pub reference_currency: u16,
    /// Markup percent the call price was derived at. It is the provenance for a
    /// `call_price_minor` that outlives the constant.
    pub call_rate: u16,
    /// Trailing span the daily scan reads for breaches, in seconds.
    pub call_window_seconds: u32,
    /// Breach seconds within that span which arm the call.
    pub call_threshold_seconds: u32,
    /// Seconds after `called_at` in which the owner must settle.
    pub call_notice_period_seconds: u32,
}

/// Shared bucket body. `bucket_nod_count` tracks unpaid membership.
/// The body is deleted when both unpaid and settled counts reach zero.
#[derive(Serialize, Deserialize)]
pub struct NodBucketState {
    pub bucket_key: B256,
    pub worldwide_day: WorldwideDay,
    pub entry_price_minor: U256,

    /// Denomination of `entry_price_minor`, propagated from the Nods in the
    /// bucket. Qualification compares the floor against the COEN day price for this
    /// currency only, and the call index is namespaced by it.
    pub reference_currency: u16,

    /// Live paid entitlements. The count decreases when one is exercised.
    #[serde(default)]
    pub settled_nods: u64,
}

impl NodBucketState {
    /// Calculates the floor that all NODs in this bucket share.
    pub fn floor_price_minor(&self) -> outbe_primitives::error::Result<U256> {
        crate::pricing::floor_price_minor(self.entry_price_minor)
            .ok_or_else(|| crate::errors::NodError::FloorPriceOverflow.into())
    }
}

/// EVM storage layout for the Nod NFT contract.
///
/// Nod item and bucket bodies and their indexes live in module-owned storage records.
/// Bucket key = keccak256(worldwide_day ++ entry_price_minor ++ reference_currency).
///
/// Uncalled buckets wait in a per-currency bitmap trie by call price, see `state::CallBins`.
///
#[storage_schema]
#[contract(addr = NOD_ADDRESS)]
pub struct NodContract {
    // slot 0: total nod items
    #[attribute(order = 0)]
    pub total_supply: outbe_primitives::storage::dsl::Value<u64>,

    // Compact reversible WWD prefix for bucket identities parked by bucket_key.
    #[attribute(order = 18)]
    pub bucket_worldwide_day: Map<B256, WorldwideDay>,

    // --- Bucket member index: lets the forfeit sweep enumerate native Nod records.
    // `WwdEntityId` occupies one storage word; IDs require no reconstruction.
    /// Authoritative unpaid member count, stored outside the shared bucket body.
    #[attribute(order = 35)]
    pub bucket_nod_count: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// `bucket_nod_key(bucket_key, index)` -> Nod id. Insertion-ordered and
    /// swap-popped, matching the call-price bin index.
    #[attribute(order = 36)]
    pub bucket_nods: Map<B256, WwdEntityId>,

    /// Nod id -> position in its bucket, for O(1) swap-remove. A Nod belongs to
    /// exactly one bucket, so one global map suffices.
    #[attribute(order = 37)]
    pub bucket_nod_index: outbe_primitives::storage::dsl::Map<WwdEntityId, u32>,

    /// `entry_price_minor * (100 + call_rate) / 100`, using the genesis-selected profile.
    /// Issuance pins this price so the daily scan need not read a bucket body to decide.
    #[attribute(order = 40)]
    pub bucket_call_price_minor: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// Snapshotted with the call price. Selects the `COEN/<iso>` VWAP series.
    #[attribute(order = 41)]
    pub bucket_currency: Map<B256, u16>,

    /// Block timestamp the bucket was force-called; `0` until called.
    #[attribute(order = 42)]
    pub bucket_called_at: outbe_primitives::storage::dsl::Map<B256, u64>,

    // --- Call terms sealed at issuance. The daily scan and the settlement-time
    // deadline check read a bucket's own copy. Thus a retuned constant leaves
    // every already-issued bucket on the terms it was issued with.
    /// Markup percent [`Self::bucket_call_price_minor`] was derived at.
    #[attribute(order = 45)]
    pub bucket_call_rate: Map<B256, u16>,

    /// Trailing span the daily scan reads for breaches, in seconds.
    #[attribute(order = 46)]
    pub bucket_call_window_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// Breach seconds within that span which arm the call.
    #[attribute(order = 47)]
    pub bucket_call_threshold_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// Seconds after `bucket_called_at` in which the owner must settle.
    #[attribute(order = 48)]
    pub bucket_call_notice_period_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// Widest `call_window_seconds` ever issued in a reference currency, in seconds. It
    /// only grows, so the trailing span the daily scan collects always covers a
    /// bucket whose sealed window outruns the current constant.
    #[attribute(order = 49)]
    pub max_call_window_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    /// First member's `issued_at`, sealed at bucket creation. VWAP history counts from its
    /// `first_full_day`. Later members inherit it, like the call terms.
    #[attribute(order = 54)]
    pub bucket_issued_at: outbe_primitives::storage::dsl::Map<B256, u64>,

    // Frozen-day call sweep: 0 = idle. At most one day waits behind the in-flight one.
    /// UTC day an unfinished call sweep is pinned to.
    #[attribute(order = 59)]
    pub call_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 60)]
    pub call_pending_day: outbe_primitives::storage::dsl::Value<u32>,

    // Call-price bin index, keyed like Gem's and Intex's.
    #[attribute(order = 61)]
    pub call_bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,
    #[attribute(order = 62)]
    pub call_bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 63)]
    pub call_bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 64)]
    pub call_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,
    #[attribute(order = 65)]
    pub call_bin_buckets: outbe_primitives::storage::dsl::Map<B256, B256>,
    /// Bucket key -> `(bin << 32) | (index + 1)` of its place in the trie; 0 = not there.
    #[attribute(order = 66)]
    pub call_bucket_slot: outbe_primitives::storage::dsl::Map<B256, u64>,

    // Called buckets, queued by the hour their notice period closes in. The trie
    // sits here and the bucket columns follow `config_profile`, whose slot genesis pins.
    #[attribute(order = 67)]
    pub expiry_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 68)]
    pub expiry_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 69)]
    pub expiry_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    /// ISO the call sweep resumes at.
    #[attribute(order = 70)]
    pub call_currency_cursor: outbe_primitives::storage::dsl::Value<u32>,
    /// `(bin << 32) | entries left`. 0 left walks the bin from the top.
    #[attribute(order = 71)]
    pub call_bin_cursor: outbe_primitives::storage::dsl::Map<u16, u64>,

    /// Call-term profile selector seeded from genesis. See `crate::config`.
    #[attribute(order = 73)]
    pub config_profile: outbe_primitives::storage::dsl::Value<u8>,

    #[attribute(order = 74)]
    pub expiry_bucket_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 75)]
    pub expiry_bucket_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 76)]
    pub expiry_bucket_at: outbe_primitives::storage::dsl::Map<B256, B256>,
    #[attribute(order = 77)]
    pub called_bucket_slot: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 78)]
    pub called_deadline: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 79)]
    pub expiry_sweep_hour: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 80)]
    pub expiry_cursor: outbe_primitives::storage::dsl::Value<u64>,

    /// Lowest sealed `call_threshold_seconds` of at least a day per reference currency.
    /// With the widest window it bounds the call prices the scan visits.
    #[attribute(order = 81)]
    pub min_call_threshold_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    /// UTC day the call sweep could not price a reference currency on. The rest of that
    /// day's pass passes it by.
    #[attribute(order = 82)]
    pub call_scan_failed_day: outbe_primitives::storage::dsl::Map<u16, u32>,
    #[attribute(order = 83)]
    pub records:
        outbe_primitives::storage::dsl::Map<WwdEntityId, crate::storage_records::StoredNod>,
    #[attribute(order = 84)]
    pub buckets: outbe_primitives::storage::dsl::Map<B256, crate::storage_records::StoredNodBucket>,
    #[attribute(order = 85)]
    pub all_ids: outbe_primitives::storage::dsl::List<WwdEntityId>,
    #[attribute(order = 86)]
    pub owner_ids: outbe_primitives::storage::dsl::Map<
        Address,
        outbe_primitives::storage::dsl::List<WwdEntityId>,
    >,
    #[attribute(order = 87)]
    pub global_id_position: outbe_primitives::storage::dsl::Map<WwdEntityId, u32>,
    #[attribute(order = 88)]
    pub owner_id_position: outbe_primitives::storage::dsl::Map<WwdEntityId, u32>,
}
