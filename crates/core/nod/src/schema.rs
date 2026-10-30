use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::NOD_ADDRESS;
use outbe_primitives::time::{first_full_day, TimeError, WorldwideDay};
use outbe_primitives::wwd_entity_id::WwdEntityId;
use serde::{Deserialize, Serialize};

/// Read-time lifecycle state. Values are part of the public Nod ABI, not storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EffectiveState {
    Issued = 0,
    Called = 1,
    Settled = 2,
    Forfeited = 3,
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
    pub nod_id: U256,

    pub owner: Address,

    pub encrypted: outbe_primitives::nod_encryption::EncryptedNodV1,

    pub worldwide_day: WorldwideDay,

    pub league_id: u16,

    pub group_key: B256,

    pub issuance_currency: u16,

    pub reference_currency: u16,

    pub issued_at: u64,

    #[serde(default)]
    pub is_settled: bool,
}

impl NodItemState {
    /// The entry the Nod was issued at, from its authenticated terms.
    #[must_use]
    pub fn entry_price_minor(&self) -> U256 {
        self.encrypted.terms.entry_price_minor
    }

    /// The first full day the Nod counts call days from. The group key binds it.
    pub fn full_day_after_issue(&self) -> Result<WorldwideDay, TimeError> {
        Ok(WorldwideDay::new(first_full_day(self.issued_at)?))
    }

    /// Identity of the group the Nod belongs to.
    pub fn group_id(&self) -> Result<WwdEntityId, TimeError> {
        Ok(WwdEntityId::from_day_and_digest(
            self.full_day_after_issue()?,
            self.group_key.0,
        ))
    }
}

/// Call terms a group seals at issuance, and the only terms every later check
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

/// Shared group record. `group_nod_count` tracks unpaid membership.
/// The record is deleted when both unpaid and settled counts reach zero.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[storage_record(exists_field = full_day_after_issue)]
pub struct NodGroupState {
    #[key]
    pub group_key: B256,
    /// The first full day every member counts call days from.
    #[attribute(order = 0)]
    pub full_day_after_issue: WorldwideDay,

    /// The currency the group's call price is quoted in. The call index is namespaced
    /// by it.
    #[attribute(order = 1)]
    pub reference_currency: u16,

    /// Live paid entitlements. The count decreases when one is mined.
    #[serde(default)]
    #[attribute(order = 2)]
    pub settled_nods: u64,
}

/// EVM storage layout for the Nod NFT contract.
///
/// Nod item and group bodies and their indexes live in module-owned storage records.
/// Group key = `call_breach::group_key(full_day_after_issue, call_price, reference_currency)`.
///
/// Uncalled groups wait in a per-currency bitmap trie by call price, see `state::CallBins`.
///
#[storage_schema]
#[contract(addr = NOD_ADDRESS)]
pub struct NodContract {
    // --- Group member index: lets the cleanup enumerate a group's unpaid Nods.
    // U256 occupies one storage word; IDs require no reconstruction.
    /// Authoritative unpaid member count, stored outside the shared group body.
    #[attribute(order = 35)]
    pub group_nod_count: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// `group_nod_key(group_key, index)` -> Nod id. Insertion-ordered and
    /// swap-popped, matching the call-price bin index.
    #[attribute(order = 36)]
    pub group_nods: Map<B256, U256>,

    /// Nod id -> position in its group, for O(1) swap-remove. A Nod belongs to
    /// exactly one group, so one global map suffices.
    #[attribute(order = 37)]
    pub group_nod_index: outbe_primitives::storage::dsl::Map<U256, u32>,

    /// `entry_price_minor * (100 + call_rate) / 100`, using the genesis-selected profile.
    /// Issuance pins this price so the daily scan need not read a group body to decide.
    #[attribute(order = 40)]
    pub group_call_price_minor: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// Snapshotted with the call price. Selects the `COEN/<iso>` VWAP series.
    #[attribute(order = 41)]
    pub group_currency: Map<B256, u16>,

    /// Block timestamp the group was force-called; `0` until called.
    #[attribute(order = 42)]
    pub group_called_at: outbe_primitives::storage::dsl::Map<B256, u64>,

    // --- Call terms sealed at issuance. The daily scan and the settlement-time
    // deadline check read a group's own copy. Thus a retuned constant leaves
    // every already-issued group on the terms it was issued with.
    /// Markup percent [`Self::group_call_price_minor`] was derived at.
    #[attribute(order = 45)]
    pub group_call_rate: Map<B256, u16>,

    /// Trailing span the daily scan reads for breaches, in seconds.
    #[attribute(order = 46)]
    pub group_call_window_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// Breach seconds within that span which arm the call.
    #[attribute(order = 47)]
    pub group_call_threshold_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// Seconds after `group_called_at` in which the owner must settle.
    #[attribute(order = 48)]
    pub group_call_notice_period_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// Widest `call_window_seconds` ever issued in a reference currency, in seconds. It
    /// only grows, so the trailing span the daily scan collects always covers a
    /// group whose sealed window outruns the current constant.
    #[attribute(order = 49)]
    pub max_call_window_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    // Frozen-day call pass: 0 = idle. Every closed day up to the newest scheduled is walked.
    /// UTC day an unfinished call pass is pinned to.
    #[attribute(order = 59)]
    pub call_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 60)]
    pub call_latest_day: outbe_primitives::storage::dsl::Value<u32>,

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
    pub call_bin_groups: outbe_primitives::storage::dsl::Map<B256, B256>,
    /// Group key -> `(bin << 32) | (index + 1)` of its place in the trie; 0 = not there.
    #[attribute(order = 66)]
    pub call_group_slot: outbe_primitives::storage::dsl::Map<B256, u64>,

    // The queue orders called groups by the UTC day when their notice period closes.
    // The trie occupies these columns. The group columns follow `config_profile`, whose slot genesis fixes.
    #[attribute(order = 67)]
    pub forfeit_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 68)]
    pub forfeit_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 69)]
    pub forfeit_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    /// ISO the call pass resumes at.
    #[attribute(order = 70)]
    pub call_currency_cursor: outbe_primitives::storage::dsl::Value<u32>,
    /// `(bin << 32) | entries left`. 0 left walks the bin from the top.
    #[attribute(order = 71)]
    pub call_bin_cursor: outbe_primitives::storage::dsl::Map<u16, u64>,

    /// Call-term profile selector seeded from genesis. See `crate::config`.
    #[attribute(order = 73)]
    pub config_profile: outbe_primitives::storage::dsl::Value<u8>,

    #[attribute(order = 74)]
    pub forfeit_day_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 75)]
    pub forfeit_day_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 76)]
    pub forfeit_day_at: outbe_primitives::storage::dsl::Map<B256, B256>,
    #[attribute(order = 77)]
    pub called_group_slot: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 78)]
    pub called_deadline: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 79)]
    pub forfeit_pass_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 80)]
    pub forfeit_cursor: outbe_primitives::storage::dsl::Value<u64>,

    /// Lowest sealed `call_threshold_seconds` of at least a day per reference currency.
    /// With the widest window it bounds the call prices the scan visits.
    #[attribute(order = 81)]
    pub min_call_threshold_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    /// UTC day the call pass could not price a reference currency on. The rest of that
    /// day's pass passes it by.
    #[attribute(order = 82)]
    pub call_scan_failed_day: outbe_primitives::storage::dsl::Map<u16, u32>,
    #[attribute(order = 83)]
    pub records: outbe_primitives::storage::dsl::Map<U256, crate::storage_records::StoredNod>,
    #[attribute(order = 84)]
    pub groups: outbe_primitives::storage::dsl::Map<B256, NodGroupState>,
    #[attribute(order = 86)]
    pub owner_ids:
        outbe_primitives::storage::dsl::Map<Address, outbe_primitives::storage::dsl::List<U256>>,
    #[attribute(order = 88)]
    pub owner_id_position: outbe_primitives::storage::dsl::Map<U256, u32>,

    /// Gratis load of the group's unpaid Nods, returned at once when it is forfeited.
    #[attribute(order = 89)]
    pub group_unpaid_load: outbe_primitives::storage::dsl::Map<B256, U256>,

    // The queue orders forfeited groups by deadline day while their unpaid Nods await deletion.
    #[attribute(order = 90)]
    pub cleanup_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 91)]
    pub cleanup_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 92)]
    pub cleanup_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 93)]
    pub cleanup_day_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 94)]
    pub cleanup_day_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 95)]
    pub cleanup_day_at: outbe_primitives::storage::dsl::Map<B256, B256>,
    #[attribute(order = 96)]
    pub cleanup_group_slot: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 97)]
    pub cleanup_deadline: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 98)]
    pub cleanup_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 99)]
    pub cleanup_cursor: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 100)]
    pub daily_phase: outbe_primitives::storage::dsl::Value<u8>,
    #[attribute(order = 101)]
    pub daily_budget: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 102)]
    pub daily_genesis_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 103)]
    pub cleanup_budget: outbe_primitives::storage::dsl::Value<U256>,

    /// Count of unclaimed Tributes belonging to a Lysis-sealed series group.
    #[attribute(order = 104)]
    pub group_unclaimed_tribute_count: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// Exact sum of `gratis_load_minor` across unclaimed Tributes in a Lysis-sealed series group.
    #[attribute(order = 105)]
    pub group_unclaimed_tribute_load: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// Set to `true` when the group's call notice deadline lapses and its unpaid/unclaimed load is forfeited.
    #[attribute(order = 106)]
    pub group_is_forfeited: outbe_primitives::storage::dsl::Map<B256, bool>,

    /// Entry price sealed on a Lysis series group (`emission_day` VWAP).
    #[attribute(order = 107)]
    pub group_entry_price_minor: outbe_primitives::storage::dsl::Map<B256, U256>,
}
