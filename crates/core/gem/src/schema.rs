use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::GEM_ADDRESS;
use outbe_primitives::series_id::SeriesId;
use outbe_primitives::time::{
    first_full_day, previous_date_key, timestamp_to_date_key, TimeError, WorldwideDay,
};
use outbe_primitives::wwd_entity_id::WwdEntityId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GemState {
    Issued = 0,
    Called = 1,
    Settled = 2,
    /// Read-time only: a Called gem past its notice period, until the cleanup deletes it.
    Forfeited = 3,
}

/// Plain issuance inputs. The call terms are not among them: the group seals them
/// from the live profile when it opens, and every later member inherits that copy.
#[derive(Debug, Clone, PartialEq)]
pub struct GemAddParams {
    pub owner: Address,
    pub promis_load_minor: U256,
    pub entry_price_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub issued_at: u64,
    /// The terms the gem's group seals if this gem opens it. `None` leaves the group
    /// out of the call index.
    pub call_terms: Option<CallTerms>,
    /// The call price marks up the VWAP of the series date: sub-series 0.
    pub market_price: bool,
}

/// One gem record. The call terms are its group's, read through [`GemView`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[storage_record(exists_field = owner)]
pub struct GemItemState {
    #[key]
    pub gem_id: U256,
    #[attribute(order = 0)]
    pub owner: Address,
    #[attribute(order = 1)]
    pub promis_load_minor: U256,
    #[attribute(order = 2)]
    pub issuance_currency: u16,
    #[attribute(order = 3)]
    pub reference_currency: u16,
    #[attribute(order = 4)]
    pub issued_at: u64,
    /// `call_breach::group_key(full_day_after_issue, call_price, reference_currency)`.
    #[attribute(order = 5)]
    pub group_key: B256,
    /// `GemState::Issued` or `GemState::Settled`. Called is the group's.
    #[attribute(order = 6)]
    pub state: u8,
    /// The group's `called_at` copied in at settlement, so a settled gem keeps it
    /// after its group is gone. `0` until then.
    #[attribute(order = 7)]
    pub called_at: u64,
    /// Block timestamp of the settlement. `0` until Settled.
    #[attribute(order = 8)]
    pub settled_at: u64,
    #[attribute(order = 9)]
    pub entry_price_minor: U256,
    /// The UTC day before the issue, in the gem's currencies.
    #[attribute(order = 10)]
    pub series_id: SeriesId,
    /// The call price of the gem inside its series: 0 for the series-date VWAP.
    #[attribute(order = 11)]
    pub sub_series_id: u16,
}

impl GemItemState {
    /// The worldwide day the gem was issued on.
    #[must_use]
    pub fn worldwide_day(&self) -> WorldwideDay {
        WorldwideDay::new(timestamp_to_date_key(self.issued_at))
    }

    /// The first full day the gem counts call days from. The group key binds it, so a
    /// gem and its group always agree on it.
    pub fn full_day_after_issue(&self) -> Result<WorldwideDay, TimeError> {
        Ok(WorldwideDay::new(first_full_day(self.issued_at)?))
    }

    /// Identity of the group body this gem belongs to.
    pub fn group_id(&self) -> Result<WwdEntityId, TimeError> {
        Ok(WwdEntityId::from_day_and_digest(
            self.full_day_after_issue()?,
            self.group_key,
        ))
    }

    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.state == GemState::Settled as u8
    }
}

/// Shared group record. The record is deleted once both the unpaid count and
/// `settled_gems` reach zero.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[storage_record(exists_field = full_day_after_issue)]
pub struct GemGroupState {
    #[key]
    pub group_key: B256,
    /// The first full day every member counts call days from.
    #[attribute(order = 0)]
    pub full_day_after_issue: WorldwideDay,
    #[attribute(order = 1)]
    pub reference_currency: u16,
    /// Paid members still holding the group open.
    #[attribute(order = 2)]
    pub settled_gems: u64,
}

impl GemGroupState {
    #[must_use]
    pub fn entity_id(&self) -> WwdEntityId {
        WwdEntityId::from_day_and_digest(self.full_day_after_issue, self.group_key)
    }
}

/// Call terms a group seals when it opens, and the only terms every later check
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

/// Everything a status read needs: the gem body, its group body and the terms the
/// group sealed on the EVM.
#[derive(Debug, Clone, PartialEq)]
pub struct GemView {
    pub item: GemItemState,
    pub group: GemGroupState,
    pub terms: CallTerms,
    /// The group's call timestamp. `0` while uncalled.
    pub group_called_at: u64,
}

impl GemView {
    /// `called_at` as the ABI reports it: the group's while the gem is unpaid, the
    /// gem's own copy once settled.
    #[must_use]
    pub fn called_at(&self) -> u64 {
        if self.item.is_settled() {
            self.item.called_at
        } else {
            self.group_called_at
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

    /// Lifecycle state without waiting for the pass: Settled stays Settled, a called
    /// gem past its deadline reads Forfeited.
    #[must_use]
    pub fn effective_state(&self, now: u64) -> u8 {
        if self.item.is_settled() {
            GemState::Settled as u8
        } else if self.group_called_at != 0 && now > self.settlement_deadline() {
            GemState::Forfeited as u8
        } else if self.group_called_at != 0 {
            GemState::Called as u8
        } else {
            GemState::Issued as u8
        }
    }
}

#[storage_schema]
#[contract(addr = GEM_ADDRESS)]
pub struct GemContract {
    // --- Group membership: unpaid gems for the cleanup, with a count,
    // positional map and reverse map for swap removal.
    #[attribute(order = 1)]
    pub group_gem_count: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// `group_member_key(group, index)` -> gem id.
    #[attribute(order = 2)]
    pub group_gems: outbe_primitives::storage::dsl::Map<B256, U256>,
    /// Gem id -> its index in its group.
    #[attribute(order = 3)]
    pub group_gem_index: outbe_primitives::storage::dsl::Map<U256, u32>,

    // --- Sealed group terms, written once when the group opens.
    #[attribute(order = 4)]
    pub group_call_price_minor: outbe_primitives::storage::dsl::Map<B256, U256>,
    #[attribute(order = 5)]
    pub group_currency: outbe_primitives::storage::dsl::Map<B256, u16>,
    #[attribute(order = 6)]
    pub group_call_rate: outbe_primitives::storage::dsl::Map<B256, u16>,
    #[attribute(order = 7)]
    pub group_call_window_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    #[attribute(order = 8)]
    pub group_call_threshold_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    #[attribute(order = 9)]
    pub group_call_notice_period_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// Block timestamp the group was called. `0` until then.
    #[attribute(order = 11)]
    pub group_called_at: outbe_primitives::storage::dsl::Map<B256, u64>,

    // --- Uncalled groups by call price, one trie per reference currency.
    #[attribute(order = 12)]
    pub call_bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,
    #[attribute(order = 13)]
    pub call_bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 14)]
    pub call_bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 15)]
    pub call_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,
    /// `bin_index_key(currency, bin, index)` -> group.
    #[attribute(order = 16)]
    pub call_bin_groups: outbe_primitives::storage::dsl::Map<B256, B256>,
    /// Group -> `(bin << 32) | (index + 1)` of its place in the trie. 0 once it left.
    #[attribute(order = 17)]
    pub call_group_slot: outbe_primitives::storage::dsl::Map<B256, u64>,
    /// `(bin << 32) | groups of that bin still to visit`. 0 = start from the lowest bin.
    #[attribute(order = 18)]
    pub call_bin_cursor: outbe_primitives::storage::dsl::Map<u16, u64>,
    /// Day a currency's window price failed to index, so the rest of that day's
    /// slices skip it instead of re-reading a window they cannot use.
    #[attribute(order = 19)]
    pub call_scan_failed_day: outbe_primitives::storage::dsl::Map<u16, u32>,

    // --- Called groups, queued by the hour their notice period closes in. Price
    // drives calling and only time drives expiry, so the two stages stay separate.
    #[attribute(order = 20)]
    pub forfeit_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 21)]
    pub forfeit_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 22)]
    pub forfeit_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    /// This value counts all slots ever used in a day.
    /// Retirement sets slots to zero without compacting them.
    #[attribute(order = 23)]
    pub forfeit_day_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 24)]
    pub forfeit_day_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// `keccak256(hour_be32 ++ slot_be32)` -> a called group's key.
    #[attribute(order = 25)]
    pub forfeit_day_at: outbe_primitives::storage::dsl::Map<B256, B256>,
    /// Group key -> `(hour << 32) | slot`. 0 = not queued.
    #[attribute(order = 26)]
    pub called_group_slot: outbe_primitives::storage::dsl::Map<B256, u64>,
    /// Held off the body so the head check costs no body load.
    #[attribute(order = 27)]
    pub called_deadline: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 28)]
    pub forfeit_pass_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 29)]
    pub forfeit_cursor: outbe_primitives::storage::dsl::Value<u64>,

    // --- Call sweep scheduling.
    /// Currency the unfinished call pass resumes at.
    #[attribute(order = 30)]
    pub call_currency_cursor: outbe_primitives::storage::dsl::Value<u32>,
    /// UTC day an unfinished call pass is pinned to, so its later slices decide
    /// against the prices it opened with. 0 = none in flight. A date key is never 0.
    #[attribute(order = 31)]
    pub call_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 32)]
    pub call_latest_day: outbe_primitives::storage::dsl::Value<u32>,

    /// Genesis parameter-profile selector (0 = auto, 1 = dev, 2 = prod). See crate::config.
    #[attribute(order = 33)]
    pub config_profile: outbe_primitives::storage::dsl::Value<u8>,

    /// Widest window ever sealed in a currency. It only grows, so the span the scan
    /// collects always covers a group whose terms outrun the live profile.
    #[attribute(order = 34)]
    pub max_call_window_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,
    /// Lowest sealed `call_threshold_seconds` of at least a day per reference currency.
    /// With the widest window it bounds the call prices the scan visits.
    #[attribute(order = 35)]
    pub min_call_threshold_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,
    #[attribute(order = 36)]
    pub records: outbe_primitives::storage::dsl::Map<U256, GemItemState>,
    #[attribute(order = 37)]
    pub groups: outbe_primitives::storage::dsl::Map<B256, GemGroupState>,
    #[attribute(order = 39)]
    pub owner_ids:
        outbe_primitives::storage::dsl::Map<Address, outbe_primitives::storage::dsl::List<U256>>,
    #[attribute(order = 41)]
    pub owner_id_position: outbe_primitives::storage::dsl::Map<U256, u32>,

    /// Promis load of the group's unpaid gems, returned at once when it is forfeited.
    #[attribute(order = 42)]
    pub group_unpaid_load: outbe_primitives::storage::dsl::Map<B256, U256>,

    // --- This queue stores forfeited groups whose unpaid gems await deletion.
    // It schedules groups by day, like the forfeit queue.
    #[attribute(order = 43)]
    pub cleanup_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 44)]
    pub cleanup_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 45)]
    pub cleanup_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 46)]
    pub cleanup_day_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 47)]
    pub cleanup_day_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 48)]
    pub cleanup_day_at: outbe_primitives::storage::dsl::Map<B256, B256>,
    #[attribute(order = 49)]
    pub cleanup_group_slot: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 50)]
    pub cleanup_deadline: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 51)]
    pub cleanup_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 52)]
    pub cleanup_cursor: outbe_primitives::storage::dsl::Value<u64>,

    #[attribute(order = 53)]
    pub daily_phase: outbe_primitives::storage::dsl::Value<u8>,
    #[attribute(order = 54)]
    pub daily_budget: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 55)]
    pub daily_genesis_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 56)]
    pub cleanup_budget: outbe_primitives::storage::dsl::Value<U256>,

    /// The number the next new call price of a series gets. 0 = none yet, so it starts at 1;
    /// the last one a series gives is 65534.
    #[attribute(order = 57)]
    pub series_next_sub_series: outbe_primitives::storage::dsl::Map<SeriesId, u16>,
    /// `sub_series_key(series, call price)` -> its sub-series number + 1. 0 = not numbered.
    #[attribute(order = 58)]
    pub sub_series_of_price: outbe_primitives::storage::dsl::Map<B256, u16>,
}

impl GemContract<'_> {
    /// `keccak256(series_id || sub_series_id_be || owner || block_number_be)`: one gem per
    /// owner, sub-series and block.
    pub fn generate_gem_id(
        series_id: SeriesId,
        sub_series_id: u16,
        owner: Address,
        block_number: u64,
    ) -> U256 {
        series_id.right_id(&[
            &sub_series_id.to_be_bytes(),
            owner.as_slice(),
            &block_number.to_be_bytes(),
        ])
    }

    /// The series of a gem issued at `issued_at`: the UTC day before it, in its currencies.
    pub fn series_of(
        issued_at: u64,
        issuance_currency: u16,
        reference_currency: u16,
    ) -> outbe_primitives::error::Result<SeriesId> {
        let day = previous_date_key(timestamp_to_date_key(issued_at))
            .map_err(crate::errors::date_step_error)?;
        SeriesId::for_pair(day, issuance_currency, reference_currency)
            .map_err(|error| crate::errors::GemError::from(error).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gem_id_hashes_the_series_sub_series_owner_and_block() {
        let series = SeriesId::from_bytes(*b"20261102-EUR-U");
        let owner = Address::repeat_byte(0x11);
        assert_eq!(
            GemContract::generate_gem_id(series, 0, owner, 1),
            U256::from_be_bytes(
                alloy_primitives::b256!(
                    "f784299ac3118dc85ddb52184520b287410841519878181a0b741fa2f865c219"
                )
                .0
            )
        );
        assert_eq!(
            GemContract::generate_gem_id(series, 1, owner, 1),
            U256::from_be_bytes(
                alloy_primitives::b256!(
                    "55f14188e745f03dcf717aafed0aecd574862369e409bf4b3ca77c12c57b6718"
                )
                .0
            )
        );
    }

    #[test]
    fn item_ids_round_trip_with_native_u256_serde_and_preserve_group_identity() {
        let mut group = GemGroupState::with_key(B256::repeat_byte(3));
        group.full_day_after_issue = WorldwideDay::new(20261016);
        group.reference_currency = 840;
        let group_json = serde_json::to_value(&group).unwrap();
        assert_eq!(
            serde_json::from_value::<GemGroupState>(group_json).unwrap(),
            group
        );

        for id in [U256::from(7), U256::MAX] {
            let mut item = GemItemState::with_key(id);
            item.owner = Address::repeat_byte(1);
            item.issued_at = WorldwideDay::new(20261015).to_timestamp_utc() + 1;
            item.group_key = group.group_key;
            let json = serde_json::to_value(&item).unwrap();
            assert_eq!(json["gem_id"], serde_json::to_value(id).unwrap());
            assert_eq!(
                serde_json::from_value::<U256>(json["gem_id"].clone()).unwrap(),
                id
            );
            let decoded: GemItemState = serde_json::from_value(json).unwrap();
            assert_eq!(decoded, item);
            assert_eq!(decoded.group_id().unwrap(), group.entity_id());
        }
    }
}
