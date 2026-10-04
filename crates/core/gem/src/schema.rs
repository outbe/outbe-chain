use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::GEM_ADDRESS;
use outbe_primitives::time::first_full_day;

/// The one type issued without a floor. Pinned to `GemTypes::Genesis` by a test in the
/// factory, which owns that enum.
pub const GENESIS_GEM_TYPE: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GemState {
    Issued = 0,
    Called = 2,
    Settled = 3,
    /// Read-time only: a Called gem past its notice period, until the sweep burns it.
    Forfeited = 4,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GemAddParams {
    pub owner: Address,
    pub gem_type: u8,
    pub promis_load_minor: U256,
    pub entry_price_minor: U256,
    pub floor_price_minor: U256,
    pub call_price_minor: U256,
    pub call_rate: u16,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub issued_at: u64,
}

/// Everything a call decision reads off a gem. Gems that share it breach on the same
/// days, so one decision covers them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BucketTerms {
    pub(crate) start_day: u32,
    pub(crate) reference_currency: u16,
    pub(crate) call_price_minor: U256,
    pub(crate) call_window_seconds: u32,
    pub(crate) call_threshold_seconds: u32,
    pub(crate) call_notice_period_seconds: u32,
}

impl BucketTerms {
    pub(crate) fn of(item: &GemData) -> Self {
        Self {
            start_day: first_full_day(item.issued_at),
            reference_currency: item.reference_currency,
            call_price_minor: item.call_price_minor,
            call_window_seconds: item.call_window_seconds,
            call_threshold_seconds: item.call_threshold_seconds,
            call_notice_period_seconds: item.call_notice_period_seconds,
        }
    }

    pub(crate) fn key(&self) -> B256 {
        let mut buf = [0u8; 4 + 2 + 32 + 4 + 4 + 4];
        buf[0..4].copy_from_slice(&self.start_day.to_be_bytes());
        buf[4..6].copy_from_slice(&self.reference_currency.to_be_bytes());
        buf[6..38].copy_from_slice(&self.call_price_minor.to_be_bytes::<32>());
        buf[38..42].copy_from_slice(&self.call_window_seconds.to_be_bytes());
        buf[42..46].copy_from_slice(&self.call_threshold_seconds.to_be_bytes());
        buf[46..50].copy_from_slice(&self.call_notice_period_seconds.to_be_bytes());
        keccak256(buf)
    }
}

#[storage_record(exists_field = owner)]
pub struct GemData {
    #[key]
    pub gem_id: U256,

    #[attribute(order = 0)]
    pub owner: Address,

    #[attribute(order = 1)]
    pub gem_type: u8,

    #[attribute(order = 2)]
    pub promis_load_minor: U256,

    #[attribute(order = 3)]
    pub entry_price_minor: U256,

    #[attribute(order = 4)]
    pub floor_price_minor: U256,

    #[attribute(order = 5)]
    pub issuance_currency: u16,

    #[attribute(order = 6)]
    pub reference_currency: u16,

    #[attribute(order = 7)]
    pub state: u8,

    #[attribute(order = 8)]
    pub issued_at: u64,

    /// Coen price level (Reference Currency) whose breach arms a Call Event.
    /// `entry_price_minor * (1 + call_rate)`; call rate is 128% for agent gems.
    #[attribute(order = 9)]
    pub call_price_minor: U256,

    /// Block timestamp when the gem was force-called; `0` until Called.
    #[attribute(order = 10, default = 0)]
    pub called_at: u64,

    /// Call Notice Period in seconds: after a Called gem passes
    /// `called_at + call_notice_period_seconds` it is forfeit-burned. Snapshot of the
    /// protocol constant at issuance.
    #[attribute(order = 11, default = 0)]
    pub call_notice_period_seconds: u32,

    /// Call-price markup percent (snapshot of `CALL_RATE` at issuance);
    /// `call_price_minor = entry_price_minor * (100 + call_rate) / 100`
    /// (128 => 2.28x).
    #[attribute(order = 12, default = 0)]
    pub call_rate: u16,

    /// Call-trigger evaluation window in seconds (snapshot of `CALL_WINDOW` at
    /// issuance); the trailing span scanned for Call Price breaches.
    #[attribute(order = 13, default = 0)]
    pub call_window_seconds: u32,

    /// Breach threshold in seconds (snapshot of `CALL_THRESHOLD` at issuance);
    /// divided by 86400 to get the required breach-day count.
    #[attribute(order = 14, default = 0)]
    pub call_threshold_seconds: u32,

    /// Block timestamp when the gem was Settled; `0` until Settled.
    #[attribute(order = 16, default = 0)]
    pub settled_at: u64,
}

impl GemData {
    pub fn effective_state(&self, now: u64) -> u8 {
        let deadline = self
            .called_at
            .saturating_add(u64::from(self.call_notice_period_seconds));
        if self.state == GemState::Called as u8 && now > deadline {
            GemState::Forfeited as u8
        } else {
            self.state
        }
    }
}

#[storage_schema]
#[contract(addr = GEM_ADDRESS)]
pub struct GemContract {
    #[attribute(order = 0)]
    pub total_supply: outbe_primitives::storage::dsl::Value<u64>,

    #[attribute(order = 1)]
    pub gem_items: outbe_primitives::storage::dsl::Map<U256, GemData>,

    #[attribute(order = 2)]
    pub owner_gem_counts: outbe_primitives::storage::dsl::Map<Address, u32>,

    #[attribute(order = 3)]
    pub owner_gem_ids: outbe_primitives::storage::dsl::Map<B256, U256>,

    #[attribute(order = 4)]
    pub all_gem_ids: outbe_primitives::storage::dsl::List<U256>,

    #[attribute(order = 5)]
    pub gem_index: outbe_primitives::storage::dsl::Map<U256, u32>,

    // --- Retired per-gem call-price bins, superseded by the bucket bins; kept for the layout.
    #[attribute(order = 11)]
    pub call_bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,

    #[attribute(order = 12)]
    pub call_bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,

    #[attribute(order = 13)]
    pub call_bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,

    #[attribute(order = 14)]
    pub call_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,

    #[attribute(order = 15)]
    pub call_bin_gems: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// Currency the unfinished call sweep resumes at.
    #[attribute(order = 16)]
    pub call_currency_cursor: outbe_primitives::storage::dsl::Value<u32>,

    #[attribute(order = 17)]
    pub call_scan_cursor: outbe_primitives::storage::dsl::Map<u16, u32>,

    // --- Called buckets, and gems called before buckets, queued by the hour their notice
    // period closes in. Calling is driven by price and expiry only by time, so the two
    // stages stay separate.
    #[attribute(order = 20)]
    pub expiry_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 21)]
    pub expiry_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 22)]
    pub expiry_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    /// Queue entry -> `(hour << 32) | slot`; 0 = not queued.
    #[attribute(order = 23)]
    pub called_bucket_slot: outbe_primitives::storage::dsl::Map<U256, u64>,
    /// Held off the record so the head check costs no record load.
    #[attribute(order = 24)]
    pub called_deadline: outbe_primitives::storage::dsl::Map<U256, u64>,

    /// UTC day an unfinished call sweep is pinned to, so its later slices decide
    /// against the prices it opened with. 0 = none in flight; a date key is never 0.
    #[attribute(order = 25)]
    pub call_sweep_day: outbe_primitives::storage::dsl::Value<u32>,

    // Genesis parameter-profile selector (0 = auto, 1 = dev, 2 = prod); see crate::config.
    #[attribute(order = 26)]
    pub config_profile: outbe_primitives::storage::dsl::Value<u8>,

    /// Widest window ever issued in a currency; it only grows, so the span the scan
    /// collects always covers a gem whose record outruns the live profile.
    #[attribute(order = 27)]
    pub max_call_window_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    /// Slots ever used in a bucket; retired ones are zeroed in place, not compacted.
    #[attribute(order = 28)]
    pub expiry_bucket_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 29)]
    pub expiry_bucket_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// `keccak256(hour_be32 ++ slot_be32)` -> queue entry: a called bucket's key, or a
    /// gem id.
    #[attribute(order = 30)]
    pub expiry_bucket_at: outbe_primitives::storage::dsl::Map<B256, U256>,
    #[attribute(order = 31)]
    pub expiry_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 32)]
    pub expiry_cursor: outbe_primitives::storage::dsl::Value<u32>,

    /// Day a currency's window price failed to index, so the rest of that day's
    /// slices skip it instead of re-reading a window they cannot use.
    #[attribute(order = 33)]
    pub call_scan_failed_day: outbe_primitives::storage::dsl::Map<u16, u32>,
    #[attribute(order = 34)]
    pub call_pending_day: outbe_primitives::storage::dsl::Value<u32>,

    /// Gem id -> its index in `owner_gem_ids`, plus one. 0 for a gem written before the
    /// field existed: burning it falls back to scanning the owner's list.
    #[attribute(order = 35)]
    pub owner_gem_position: outbe_primitives::storage::dsl::Map<U256, u32>,

    // --- Call buckets: gems issued for the same first full day under the same call terms
    // breach together, so they are called and forfeited together.
    /// Gem id -> its bucket; zero for a gem issued before buckets existed.
    #[attribute(order = 36)]
    pub gem_bucket: outbe_primitives::storage::dsl::Map<U256, B256>,
    #[attribute(order = 37)]
    pub bucket_gem_count: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// `bucket_member_key(bucket, index)` -> gem id; swap-popped.
    #[attribute(order = 38)]
    pub bucket_gems: outbe_primitives::storage::dsl::Map<B256, U256>,
    /// Gem id -> its index in its bucket.
    #[attribute(order = 39)]
    pub bucket_gem_index: outbe_primitives::storage::dsl::Map<U256, u32>,
    #[attribute(order = 40)]
    pub bucket_call_price_minor: outbe_primitives::storage::dsl::Map<B256, U256>,
    #[attribute(order = 41)]
    pub bucket_currency: outbe_primitives::storage::dsl::Map<B256, u16>,
    /// First UTC day whose price counts towards the bucket's call.
    #[attribute(order = 42)]
    pub bucket_start_day: outbe_primitives::storage::dsl::Map<B256, u32>,
    #[attribute(order = 43)]
    pub bucket_call_window_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    #[attribute(order = 44)]
    pub bucket_call_threshold_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    #[attribute(order = 45)]
    pub bucket_call_notice_period_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// Block timestamp the bucket was called; `0` until then.
    #[attribute(order = 46)]
    pub bucket_called_at: outbe_primitives::storage::dsl::Map<B256, u64>,

    // --- Uncalled buckets by call price, one trie per reference currency.
    #[attribute(order = 47)]
    pub bucket_bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,
    #[attribute(order = 48)]
    pub bucket_bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 49)]
    pub bucket_bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 50)]
    pub bucket_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,
    /// `bin_index_key(currency, bin, index)` -> bucket.
    #[attribute(order = 51)]
    pub bucket_bin_at: outbe_primitives::storage::dsl::Map<B256, B256>,
    /// Bucket -> its index in its bin, plus one; 0 once it left the trie.
    #[attribute(order = 52)]
    pub bucket_bin_index: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// `(bin << 32) | buckets of that bin still to visit`; 0 = start from the lowest bin.
    #[attribute(order = 53)]
    pub bucket_scan_cursor: outbe_primitives::storage::dsl::Map<u16, u64>,

    /// Genesis gem id issued while no Worldwide Day existed. `1` is the
    /// issuance-time privilege; an absent key stays `0` and is never filled in
    /// for a gem issued earlier. A new Genesis issue writes `0` or `1` for that
    /// call, including over an id that burn freed in the same block. Not a Qualified
    /// state and not part of `GemData`.
    #[attribute(order = 54)]
    pub issued_before_first_wwd: outbe_primitives::storage::dsl::Map<U256, u8>,
}

impl GemContract<'_> {
    /// `gem_id = keccak256("gem" || owner || amount_be || block_number_be)`.
    /// `amount` is the gem's `promis_load_minor` (reward principal).
    pub fn generate_gem_id(owner: Address, amount: U256, block_number: u64) -> U256 {
        let mut buf = [0u8; 3 + 20 + 32 + 8];
        buf[0..3].copy_from_slice(b"gem");
        buf[3..23].copy_from_slice(owner.as_slice());
        buf[23..55].copy_from_slice(&amount.to_be_bytes::<32>());
        buf[55..63].copy_from_slice(&block_number.to_be_bytes());
        U256::from_be_bytes(keccak256(buf).0)
    }
}
