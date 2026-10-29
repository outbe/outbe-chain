use alloy_primitives::U256;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::EMISSION_ADDRESS;
use outbe_primitives::time::WorldwideDay as WorldwideDayKey;

/// Lifecycle state of an `EmissionDay` (OIP-00348).
pub mod emission_day_state {
    pub const PENDING: u8 = 0;
    pub const SEALED: u8 = 1;
    pub const PROCESSING: u8 = 2;
    pub const COMPLETED: u8 = 3;
}

/// Lifecycle state of the Metadosis phase of an `EmissionDay` (OIP-00348).
pub mod metadosis_state {
    pub const PENDING: u8 = 0;
    pub const COMPLETED: u8 = 1;
    pub const CANCELLED: u8 = 2;
    pub const FAILED: u8 = 3;
}

/// Canonical 6-slot packed `EmissionDay` record stored at `EMISSION_ADDRESS` (`0x...100D`).
///
/// Greedy packing layout across 6 EVM storage slots:
/// - **Slot 0:** `wwd` (`0..4`), `metadosis_start` (`4..12`), `state` (`12..13`),
///   `metadosis_state` (`13..14`), `emission_limit` (`14..30`).
/// - **Slot 1:** `metadosis_limit` (`0..16`), `lysis_limit` (`16..32`).
/// - **Slot 2:** `desis_limit` (`0..16`), `validators` (`16..32`).
/// - **Slot 3:** `waa` (`0..16`), `sra` (`16..32`).
/// - **Slot 4:** `cca` (`0..16`), `nods` (`16..32`).
/// - **Slot 5:** `promis_limit` (`0..16`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[storage_record(packed, exists_field = metadosis_start)]
pub struct EmissionDay {
    #[key]
    pub day: u32,

    #[attribute(order = 0)]
    pub wwd: WorldwideDayKey,

    #[attribute(order = 1)]
    pub metadosis_start: u64,

    #[attribute(order = 2, default = emission_day_state::PENDING)]
    pub state: u8,

    #[attribute(order = 3, default = metadosis_state::PENDING)]
    pub metadosis_state: u8,

    /// Six-decimal protocol COEN minor units (1_000_000 minor units = 1 COEN).
    #[attribute(order = 4, default = 0)]
    pub emission_limit: u128,

    #[attribute(order = 5, default = 0)]
    pub metadosis_limit: u128,

    #[attribute(order = 6, default = 0)]
    pub lysis_limit: u128,

    #[attribute(order = 7, default = 0)]
    pub desis_limit: u128,

    #[attribute(order = 8, default = 0)]
    pub validators: u128,

    #[attribute(order = 9, default = 0)]
    pub waa: u128,

    #[attribute(order = 10, default = 0)]
    pub sra: u128,

    #[attribute(order = 11, default = 0)]
    pub cca: u128,

    #[attribute(order = 12, default = 0)]
    pub nods: u128,

    #[attribute(order = 13, default = 0)]
    pub promis_limit: u128,
}

impl EmissionDay {
    /// Creates a new `Pending` `EmissionDay` shell with all monetary fields set to 0.
    #[inline]
    pub const fn new_pending(day: u32, wwd: WorldwideDayKey, metadosis_start: u64) -> Self {
        Self {
            day,
            wwd,
            metadosis_start,
            state: emission_day_state::PENDING,
            metadosis_state: metadosis_state::PENDING,
            emission_limit: 0,
            metadosis_limit: 0,
            lysis_limit: 0,
            desis_limit: 0,
            validators: 0,
            waa: 0,
            sra: 0,
            cca: 0,
            nods: 0,
            promis_limit: 0,
        }
    }

    /// Creates an `EmissionDay` initialized with a single `limit` in `emission_limit` and `metadosis_limit`.
    #[inline]
    pub const fn new_with_limit(
        day: u32,
        wwd: WorldwideDayKey,
        metadosis_start: u64,
        limit: u128,
        metadosis_state: u8,
    ) -> Self {
        Self {
            day,
            wwd,
            metadosis_start,
            state: emission_day_state::PENDING,
            metadosis_state,
            emission_limit: limit,
            metadosis_limit: limit,
            lysis_limit: 0,
            desis_limit: 0,
            validators: 0,
            waa: 0,
            sra: 0,
            cca: 0,
            nods: 0,
            promis_limit: 0,
        }
    }

    /// Returns `emission_limit` widened to `U256` in six-decimal minor units.
    #[inline]
    pub fn emission_limit_minor(&self) -> U256 {
        U256::from(self.metadosis_limit.max(self.emission_limit))
    }

    /// Returns `metadosis_limit` widened to `U256` in six-decimal minor units.
    #[inline]
    pub fn metadosis_limit_minor(&self) -> U256 {
        U256::from(self.metadosis_limit)
    }

    /// Computes the sum of the 6 landing buckets (`validators + waa + sra + cca + nods + promis_limit`).
    pub fn landed_sum(&self) -> Option<u128> {
        self.validators
            .checked_add(self.waa)?
            .checked_add(self.sra)?
            .checked_add(self.cca)?
            .checked_add(self.nods)?
            .checked_add(self.promis_limit)
    }
}

/// Native EmissionDay ledger contract at `EMISSION_ADDRESS`.
#[storage_schema]
#[contract(addr = EMISSION_ADDRESS)]
pub struct EmissionContract {
    #[attribute(order = 0)]
    pub emission_days: outbe_primitives::storage::dsl::Map<u32, EmissionDay>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::{
        dsl::StorageRecord, hashmap::HashMapStorageProvider, types::unpack_from_word, StorageHandle,
    };

    #[test]
    fn emission_days_map_base_is_slot_zero_and_occupies_six_slots() {
        let mut provider = HashMapStorageProvider::new(1);
        let contract = EmissionContract::new(StorageHandle::new(&mut provider));
        assert_eq!(contract.emission_days.base_slot(), U256::from(0));
        assert_eq!(EmissionDay::SLOTS, 6);
    }

    #[test]
    fn emission_day_six_slot_packed_layout_round_trips_and_supports_empty_days() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let contract = EmissionContract::new(storage.clone());

        let empty_day = EmissionDay::new_pending(20261001, WorldwideDayKey::new(0), 1_790_899_201);
        contract.emission_days.create(&empty_day).unwrap();
        let loaded_empty = contract
            .emission_days
            .get(20261001)
            .unwrap()
            .expect("empty day with wwd=0 and metadosis_start>0 must exist");
        assert_eq!(loaded_empty.wwd, WorldwideDayKey::new(0));
        assert_eq!(loaded_empty.metadosis_start, 1_790_899_201);
        assert_eq!(loaded_empty.state, emission_day_state::PENDING);
        assert_eq!(loaded_empty.metadosis_state, metadosis_state::PENDING);
        assert_eq!(loaded_empty.emission_limit, 0);

        let populated = EmissionDay {
            day: 20261022,
            wwd: WorldwideDayKey::new(20261001),
            metadosis_start: 1_792_713_601,
            state: emission_day_state::COMPLETED,
            metadosis_state: metadosis_state::COMPLETED,
            emission_limit: 1000,
            metadosis_limit: 840,
            lysis_limit: 300,
            desis_limit: 540,
            validators: 40,
            waa: 40,
            sra: 40,
            cca: 40,
            nods: 290,
            promis_limit: 550,
        };
        assert_eq!(populated.landed_sum(), Some(populated.emission_limit));
        contract.emission_days.create(&populated).unwrap();

        // Verify Slot 0 exact byte offsets:
        // 0..4: wwd, 4..12: metadosis_start, 12: state, 13: metadosis_state, 14..30: emission_limit
        let word0 = outbe_primitives::storage::types::Mapping::<u32, U256>::new(
            U256::from(0),
            EMISSION_ADDRESS,
            storage.clone(),
        )
        .read(&20261022)
        .unwrap();
        assert_eq!(unpack_from_word::<u32>(word0, 0), 20261001);
        assert_eq!(unpack_from_word::<u64>(word0, 4), 1_792_713_601);
        assert_eq!(
            unpack_from_word::<u8>(word0, 12),
            emission_day_state::COMPLETED
        );
        assert_eq!(
            unpack_from_word::<u8>(word0, 13),
            metadosis_state::COMPLETED
        );
        assert_eq!(unpack_from_word::<u128>(word0, 14), 1000);

        let loaded = contract.emission_days.get(20261022).unwrap().unwrap();
        assert_eq!(loaded.day, populated.day);
        assert_eq!(loaded.wwd, populated.wwd);
        assert_eq!(loaded.metadosis_start, populated.metadosis_start);
        assert_eq!(loaded.state, populated.state);
        assert_eq!(loaded.metadosis_state, populated.metadosis_state);
        assert_eq!(loaded.emission_limit, populated.emission_limit);
        assert_eq!(loaded.metadosis_limit, populated.metadosis_limit);
        assert_eq!(loaded.lysis_limit, populated.lysis_limit);
        assert_eq!(loaded.desis_limit, populated.desis_limit);
        assert_eq!(loaded.validators, populated.validators);
        assert_eq!(loaded.waa, populated.waa);
        assert_eq!(loaded.sra, populated.sra);
        assert_eq!(loaded.cca, populated.cca);
        assert_eq!(loaded.nods, populated.nods);
        assert_eq!(loaded.promis_limit, populated.promis_limit);
    }
}
