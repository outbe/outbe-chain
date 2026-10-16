use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::GEM_ADDRESS;
use outbe_primitives::time::{timestamp_to_date_key, WorldwideDay};
use outbe_primitives::wwd_entity_id::WwdEntityId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GemState {
    Issued = 0,
    Called = 1,
    Settled = 2,
    /// Read-time only: a Called gem past its notice period, until the sweep burns it.
    Forfeited = 3,
}

/// Plain issuance inputs. The call terms are not among them: the bucket seals them
/// from the live profile when it opens, and every later member inherits that copy.
#[derive(Debug, Clone, PartialEq)]
pub struct GemAddParams {
    pub owner: Address,
    pub gem_type: u8,
    pub promis_load_minor: U256,
    pub entry_price_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub issued_at: u64,
    /// The terms the gem's bucket seals if this gem opens it. `None` leaves the bucket
    /// out of the call index.
    pub call_terms: Option<CallTerms>,
}

/// One gem record. The entry price and the call terms are the bucket's, read
/// through [`GemView`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[storage_record(exists_field = owner)]
pub struct GemItemState {
    #[key]
    pub gem_id: WwdEntityId,
    #[attribute(order = 0)]
    pub owner: Address,
    #[attribute(order = 1)]
    pub gem_type: u8,
    #[attribute(order = 2)]
    pub promis_load_minor: U256,
    #[attribute(order = 3)]
    pub issuance_currency: u16,
    #[attribute(order = 4)]
    pub reference_currency: u16,
    #[attribute(order = 5)]
    pub issued_at: u64,
    /// `keccak(wwd ‖ entry_price ‖ reference_currency)`, see `crate::identity`.
    #[attribute(order = 6)]
    pub bucket_key: B256,
    /// `GemState::Issued` or `GemState::Settled`. Called is the bucket's.
    #[attribute(order = 7)]
    pub state: u8,
    /// The bucket's `called_at` copied in at settlement, so a settled gem keeps it
    /// after its bucket is gone. `0` until then.
    #[attribute(order = 8)]
    pub called_at: u64,
    /// Block timestamp of the settlement. `0` until Settled.
    #[attribute(order = 9)]
    pub settled_at: u64,
}

impl GemItemState {
    /// The worldwide day the gem was issued on. The bucket key binds this day, so a
    /// gem and its bucket always agree on it.
    #[must_use]
    pub fn worldwide_day(&self) -> WorldwideDay {
        WorldwideDay::new(timestamp_to_date_key(self.issued_at))
    }

    /// Identity of the bucket body this gem belongs to.
    #[must_use]
    pub fn bucket_id(&self) -> WwdEntityId {
        WwdEntityId::from_day_and_digest(self.worldwide_day(), self.bucket_key)
    }

    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.state == GemState::Settled as u8
    }
}

/// Shared bucket record. The record is deleted once both the unpaid count and
/// `settled_gems` reach zero.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[storage_record(exists_field = worldwide_day)]
pub struct GemBucketState {
    #[key]
    pub bucket_key: B256,
    /// Worldwide day of the first member. Later members are issued on the same day.
    #[attribute(order = 0)]
    pub worldwide_day: WorldwideDay,
    #[attribute(order = 1)]
    pub entry_price_minor: U256,
    #[attribute(order = 2)]
    pub reference_currency: u16,
    /// Paid members still holding the bucket open.
    #[attribute(order = 3)]
    pub settled_gems: u64,
}

impl GemBucketState {
    #[must_use]
    pub fn entity_id(&self) -> WwdEntityId {
        WwdEntityId::from_day_and_digest(self.worldwide_day, self.bucket_key)
    }
}

/// Call terms a bucket seals when it opens, and the only terms every later check
/// reads. Grouped so same-typed numbers cannot swap places on the way to storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallTerms {
    /// `entry_price_minor x (100 + call_rate) / 100`.
    pub call_price_minor: U256,
    pub reference_currency: u16,
    pub call_rate: u16,
    pub call_window_seconds: u32,
    pub call_threshold_seconds: u32,
    pub call_notice_period_seconds: u32,
}

/// Everything a status read needs: the gem body, its bucket body and the terms the
/// bucket sealed on the EVM.
#[derive(Debug, Clone, PartialEq)]
pub struct GemView {
    pub item: GemItemState,
    pub bucket: GemBucketState,
    pub terms: CallTerms,
    /// The bucket's call timestamp. `0` while uncalled.
    pub bucket_called_at: u64,
}

impl GemView {
    /// `called_at` as the ABI reports it: the bucket's while the gem is unpaid, the
    /// gem's own copy once settled.
    #[must_use]
    pub fn called_at(&self) -> u64 {
        if self.item.is_settled() {
            self.item.called_at
        } else {
            self.bucket_called_at
        }
    }

    /// Timestamp a called gem must be settled by, or `0` while it is not called.
    #[must_use]
    pub fn settlement_deadline(&self) -> u64 {
        match self.called_at() {
            0 => 0,
            called_at => {
                crate::api::settlement_deadline_of(called_at, self.terms.call_notice_period_seconds)
            }
        }
    }

    /// Lifecycle state without waiting for the sweep: Settled stays Settled, a called
    /// gem past its deadline reads Forfeited.
    #[must_use]
    pub fn effective_state(&self, now: u64) -> u8 {
        if self.item.is_settled() {
            GemState::Settled as u8
        } else if self.bucket_called_at != 0 && now > self.settlement_deadline() {
            GemState::Forfeited as u8
        } else if self.bucket_called_at != 0 {
            GemState::Called as u8
        } else {
            GemState::Issued as u8
        }
    }
}

#[storage_schema]
#[contract(addr = GEM_ADDRESS)]
pub struct GemContract {
    #[attribute(order = 0)]
    pub total_supply: outbe_primitives::storage::dsl::Value<u64>,

    // --- Bucket membership: unpaid gems for the forfeit sweep, with a count,
    // positional map and reverse map for swap removal.
    #[attribute(order = 1)]
    pub bucket_gem_count: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// `bucket_member_key(bucket, index)` -> gem id.
    #[attribute(order = 2)]
    pub bucket_gems: outbe_primitives::storage::dsl::Map<B256, U256>,
    /// Gem id -> its index in its bucket.
    #[attribute(order = 3)]
    pub bucket_gem_index: outbe_primitives::storage::dsl::Map<U256, u32>,

    // --- Sealed bucket terms, written once when the bucket opens.
    #[attribute(order = 4)]
    pub bucket_call_price_minor: outbe_primitives::storage::dsl::Map<B256, U256>,
    #[attribute(order = 5)]
    pub bucket_currency: outbe_primitives::storage::dsl::Map<B256, u16>,
    #[attribute(order = 6)]
    pub bucket_call_rate: outbe_primitives::storage::dsl::Map<B256, u16>,
    #[attribute(order = 7)]
    pub bucket_call_window_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    #[attribute(order = 8)]
    pub bucket_call_threshold_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    #[attribute(order = 9)]
    pub bucket_call_notice_period_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// Issuance timestamp of the first member. `first_full_day` of it starts the
    /// breach count of the whole bucket.
    #[attribute(order = 10)]
    pub bucket_issued_at: outbe_primitives::storage::dsl::Map<B256, u64>,
    /// Block timestamp the bucket was called. `0` until then.
    #[attribute(order = 11)]
    pub bucket_called_at: outbe_primitives::storage::dsl::Map<B256, u64>,

    // --- Uncalled buckets by call price, one trie per reference currency.
    #[attribute(order = 12)]
    pub call_bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,
    #[attribute(order = 13)]
    pub call_bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 14)]
    pub call_bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 15)]
    pub call_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,
    /// `bin_index_key(currency, bin, index)` -> bucket.
    #[attribute(order = 16)]
    pub call_bin_buckets: outbe_primitives::storage::dsl::Map<B256, B256>,
    /// Bucket -> `(bin << 32) | (index + 1)` of its place in the trie. 0 once it left.
    #[attribute(order = 17)]
    pub call_bucket_slot: outbe_primitives::storage::dsl::Map<B256, u64>,
    /// `(bin << 32) | buckets of that bin still to visit`. 0 = start from the lowest bin.
    #[attribute(order = 18)]
    pub call_bin_cursor: outbe_primitives::storage::dsl::Map<u16, u64>,
    /// Day a currency's window price failed to index, so the rest of that day's
    /// slices skip it instead of re-reading a window they cannot use.
    #[attribute(order = 19)]
    pub call_scan_failed_day: outbe_primitives::storage::dsl::Map<u16, u32>,

    // --- Called buckets, queued by the hour their notice period closes in. Price
    // drives calling and only time drives expiry, so the two stages stay separate.
    #[attribute(order = 20)]
    pub expiry_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 21)]
    pub expiry_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 22)]
    pub expiry_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    /// Slots ever used in a bucket. Retired ones are zeroed in place, not compacted.
    #[attribute(order = 23)]
    pub expiry_bucket_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 24)]
    pub expiry_bucket_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// `keccak256(hour_be32 ++ slot_be32)` -> a called bucket's key.
    #[attribute(order = 25)]
    pub expiry_bucket_at: outbe_primitives::storage::dsl::Map<B256, B256>,
    /// Bucket key -> `(hour << 32) | slot`. 0 = not queued.
    #[attribute(order = 26)]
    pub called_bucket_slot: outbe_primitives::storage::dsl::Map<B256, u64>,
    /// Held off the body so the head check costs no body load.
    #[attribute(order = 27)]
    pub called_deadline: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 28)]
    pub expiry_sweep_hour: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 29)]
    pub expiry_cursor: outbe_primitives::storage::dsl::Value<u64>,

    // --- Call sweep scheduling.
    /// Currency the unfinished call sweep resumes at.
    #[attribute(order = 30)]
    pub call_currency_cursor: outbe_primitives::storage::dsl::Value<u32>,
    /// UTC day an unfinished call sweep is pinned to, so its later slices decide
    /// against the prices it opened with. 0 = none in flight. A date key is never 0.
    #[attribute(order = 31)]
    pub call_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 32)]
    pub call_pending_day: outbe_primitives::storage::dsl::Value<u32>,

    /// Genesis parameter-profile selector (0 = auto, 1 = dev, 2 = prod). See crate::config.
    #[attribute(order = 33)]
    pub config_profile: outbe_primitives::storage::dsl::Value<u8>,

    /// Widest window ever sealed in a currency. It only grows, so the span the scan
    /// collects always covers a bucket whose terms outrun the live profile.
    #[attribute(order = 34)]
    pub max_call_window_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,
    /// Lowest sealed `call_threshold_seconds` of at least a day per reference currency.
    /// With the widest window it bounds the call prices the scan visits.
    #[attribute(order = 35)]
    pub min_call_threshold_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,
    #[attribute(order = 36)]
    pub records: outbe_primitives::storage::dsl::Map<WwdEntityId, GemItemState>,
    #[attribute(order = 37)]
    pub buckets: outbe_primitives::storage::dsl::Map<B256, GemBucketState>,
    #[attribute(order = 38)]
    pub all_ids: outbe_primitives::storage::dsl::List<WwdEntityId>,
    #[attribute(order = 39)]
    pub owner_ids: outbe_primitives::storage::dsl::Map<
        Address,
        outbe_primitives::storage::dsl::List<WwdEntityId>,
    >,
    #[attribute(order = 40)]
    pub global_id_position: outbe_primitives::storage::dsl::Map<WwdEntityId, u32>,
    #[attribute(order = 41)]
    pub owner_id_position: outbe_primitives::storage::dsl::Map<WwdEntityId, u32>,
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
