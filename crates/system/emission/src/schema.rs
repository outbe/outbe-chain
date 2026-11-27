use alloy_primitives::U256;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_system_common::addresses::EMISSION_ADDRESS;
use outbe_system_common::time::WorldwideDay as WorldwideDayKey;

/// Lifecycle state of an `EmissionDay` (OIP-00348).
pub mod emission_day_state {
    pub const PENDING: u8 = 0;
    pub const PROCESSING: u8 = 1;
    pub const COMPLETED: u8 = 2;
}

/// Lifecycle state of the Metadosis phase of an `EmissionDay`.
pub mod metadosis_state {
    pub const PENDING: u8 = 0;
    pub const COMPLETED: u8 = 1;
    pub const FAILED: u8 = 2;
}

/// Lifecycle state of the Desis phase of an `EmissionDay`.
pub mod desis_state {
    pub const PENDING: u8 = 0;
    pub const COMPLETED: u8 = 1;
    pub const CANCELED: u8 = 2;
    pub const FAILED: u8 = 3;
}

/// The record of one emission day: the limit of each part, set at 00:00 E−1, and what each
/// part gave out. Amounts are six-decimal protocol COEN minor units.
///
/// Greedy packing layout across 9 EVM storage slots:
/// - **Slot 0:** `wwd`, `metadosis_start`, `state`, `metadosis_state`, `desis_state`, `emission_limit`.
/// - **Slot 1:** `validators_limit`, `cca_limit`. **Slot 2:** `waa_limit`, `sra_limit`.
/// - **Slot 3:** `metadosis_limit`, `lysis_limit`. **Slot 4:** `desis_limit`, `nominal_amount`.
/// - **Slot 5:** `validators`, `waa`. **Slot 6:** `sra`, `cca`. **Slot 7:** `nods`, `intexes`.
/// - **Slot 8:** `promis_limit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[storage_record(packed, exists_field = metadosis_start)]
pub struct EmissionDay {
    #[key]
    pub day: u32,

    #[attribute(order = 0)]
    #[field(bytes = 4)]
    pub wwd: WorldwideDayKey,

    #[attribute(order = 1)]
    pub metadosis_start: u64,

    #[attribute(order = 2, default = emission_day_state::PENDING)]
    pub state: u8,

    #[attribute(order = 3, default = metadosis_state::PENDING)]
    pub metadosis_state: u8,

    #[attribute(order = 4, default = desis_state::PENDING)]
    pub desis_state: u8,

    #[attribute(order = 5, default = 0)]
    pub emission_limit: u128,

    #[attribute(order = 6, default = 0)]
    pub validators_limit: u128,

    #[attribute(order = 7, default = 0)]
    pub cca_limit: u128,

    #[attribute(order = 8, default = 0)]
    pub waa_limit: u128,

    #[attribute(order = 9, default = 0)]
    pub sra_limit: u128,

    #[attribute(order = 10, default = 0)]
    pub metadosis_limit: u128,

    /// The smaller of 32% of `nominal_amount` and `metadosis_limit`: the loads the Nods of the
    /// day can take at most.
    #[attribute(order = 11, default = 0)]
    pub lysis_limit: u128,

    #[attribute(order = 12, default = 0)]
    pub desis_limit: u128,

    /// The sum of the nominal amount of the Nods of the day.
    #[attribute(order = 13, default = 0)]
    pub nominal_amount: u128,

    #[attribute(order = 14, default = 0)]
    pub validators: u128,

    #[attribute(order = 15, default = 0)]
    pub waa: u128,

    #[attribute(order = 16, default = 0)]
    pub sra: u128,

    #[attribute(order = 17, default = 0)]
    pub cca: u128,

    #[attribute(order = 18, default = 0)]
    pub nods: u128,

    /// What the Intex auction of the day gave to its winners.
    #[attribute(order = 19, default = 0)]
    pub intexes: u128,

    /// The net amount the day sent to the Promis Limit. Negative when the auction of the day
    /// took more from the Promis Limit than the day sent to it.
    #[attribute(order = 20, default = 0)]
    #[field(bytes = 16)]
    pub promis_limit: i128,
}

impl EmissionDay {
    /// A `Pending` record with the limits of its parts and no results.
    pub fn new(
        day: u32,
        wwd: WorldwideDayKey,
        metadosis_start: u64,
        emission_limit: u128,
        shares: [u128; 5],
    ) -> Self {
        let [validators_limit, cca_limit, waa_limit, sra_limit, metadosis_limit] = shares;
        Self {
            day,
            wwd,
            metadosis_start,
            state: emission_day_state::PENDING,
            metadosis_state: metadosis_state::PENDING,
            desis_state: desis_state::PENDING,
            emission_limit,
            validators_limit,
            cca_limit,
            waa_limit,
            sra_limit,
            metadosis_limit,
            lysis_limit: 0,
            desis_limit: 0,
            nominal_amount: 0,
            validators: 0,
            waa: 0,
            sra: 0,
            cca: 0,
            nods: 0,
            intexes: 0,
            promis_limit: 0,
        }
    }

    /// Returns `metadosis_limit` widened to `U256` in six-decimal minor units.
    #[inline]
    pub fn metadosis_limit_minor(&self) -> U256 {
        U256::from(self.metadosis_limit)
    }

    /// `validators + waa + sra + cca + nods + intexes + promis_limit`, which the rule of the
    /// record keeps equal to `emission_limit`.
    pub fn landed_sum(&self) -> Option<i128> {
        [
            self.validators,
            self.waa,
            self.sra,
            self.cca,
            self.nods,
            self.intexes,
        ]
        .into_iter()
        .try_fold(self.promis_limit, |sum, amount| {
            sum.checked_add(i128::try_from(amount).ok()?)
        })
    }
}

/// Native EmissionDay ledger contract at `EMISSION_ADDRESS`.
#[storage_schema]
#[contract(addr = EMISSION_ADDRESS)]
pub struct EmissionContract {
    #[attribute(order = 0)]
    pub emission_days: outbe_primitives::storage::dsl::Map<u32, EmissionDay>,

    /// The Nod loads that the Metadosis limits of past days did not cover, in minor units.
    #[attribute(order = 1)]
    pub excess_nod_loads_minor: outbe_primitives::storage::dsl::Value<U256>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::{
        dsl::StorageRecord, hashmap::HashMapStorageProvider, types::unpack_from_word, StorageHandle,
    };

    fn populated() -> EmissionDay {
        EmissionDay {
            day: 20261022,
            wwd: WorldwideDayKey::new(20261021),
            metadosis_start: 1_792_713_600,
            state: emission_day_state::COMPLETED,
            metadosis_state: metadosis_state::COMPLETED,
            desis_state: desis_state::COMPLETED,
            emission_limit: 1000,
            validators_limit: 40,
            cca_limit: 40,
            waa_limit: 40,
            sra_limit: 40,
            metadosis_limit: 840,
            lysis_limit: 320,
            desis_limit: 900,
            nominal_amount: 1000,
            validators: 40,
            waa: 39,
            sra: 40,
            cca: 40,
            nods: 320,
            intexes: 700,
            promis_limit: -179,
        }
    }

    #[test]
    fn emission_days_map_base_is_slot_zero_and_occupies_nine_slots() {
        let mut provider = HashMapStorageProvider::new(1);
        let contract = EmissionContract::new(StorageHandle::new(&mut provider));
        assert_eq!(contract.emission_days.base_slot(), U256::from(0));
        assert_eq!(EmissionDay::SLOTS, 9);
    }

    #[test]
    fn the_record_round_trips_with_a_negative_promis_limit() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let contract = EmissionContract::new(storage.clone());
        let record = populated();
        assert_eq!(
            record.landed_sum(),
            Some(i128::try_from(record.emission_limit).unwrap())
        );
        contract.emission_days.create(&record).unwrap();

        let word0 = outbe_primitives::storage::types::Mapping::<u32, U256>::new(
            U256::from(0),
            EMISSION_ADDRESS,
            storage,
        )
        .read(&record.day)
        .unwrap();
        assert_eq!(unpack_from_word::<u32>(word0, 0), 20261021);
        assert_eq!(unpack_from_word::<u64>(word0, 4), 1_792_713_600);
        assert_eq!(unpack_from_word::<u8>(word0, 14), desis_state::COMPLETED);
        assert_eq!(unpack_from_word::<u128>(word0, 15), 1000);

        assert_eq!(
            contract.emission_days.get(record.day).unwrap(),
            Some(record)
        );
    }

    #[test]
    fn a_new_record_exists_with_its_limits_and_no_results() {
        let mut provider = HashMapStorageProvider::new(1);
        let contract = EmissionContract::new(StorageHandle::new(&mut provider));
        let record = EmissionDay::new(
            20261001,
            WorldwideDayKey::new(0),
            1_790_899_200,
            100,
            [4, 4, 4, 4, 84],
        );
        contract.emission_days.create(&record).unwrap();
        let loaded = contract.emission_days.get(20261001).unwrap().unwrap();
        assert_eq!(loaded.state, emission_day_state::PENDING);
        assert_eq!(loaded.desis_state, desis_state::PENDING);
        assert_eq!(loaded.metadosis_limit, 84);
        assert_eq!(loaded.promis_limit, 0);
    }
}
