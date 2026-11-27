//! Read-only view precompile for the Emission module (`IEmission` at `EMISSION_ADDRESS`).

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;
use outbe_primitives::dispatch::{dispatch_call, view};
use outbe_primitives::error::Result;

use crate::schema::EmissionContract;

/// Selectors on this precompile that accept native value.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/IEmission.sol"
    );
}
pub use abi::IEmission;

/// Dispatches an ABI-encoded call to the Emission precompile (view-only).
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IEmission::IEmissionCalls::abi_decode, |call| {
        let contract = EmissionContract::new(storage);
        use IEmission::IEmissionCalls::*;
        match call {
            getEmissionDay(c) => view(c, |c| {
                let Some(record) = contract.emission_days.get(c.day)? else {
                    return Err(outbe_primitives::storage::dsl::missing_record_err(
                        "EmissionDay",
                    ));
                };
                Ok(IEmission::EmissionDayData {
                    day: record.day,
                    wwd: record.wwd.value(),
                    metadosisStart: record.metadosis_start,
                    state: record.state,
                    metadosisState: record.metadosis_state,
                    desisState: record.desis_state,
                    emissionLimit: record.emission_limit,
                    validatorsLimit: record.validators_limit,
                    ccaLimit: record.cca_limit,
                    waaLimit: record.waa_limit,
                    sraLimit: record.sra_limit,
                    metadosisLimit: record.metadosis_limit,
                    lysisLimit: record.lysis_limit,
                    desisLimit: record.desis_limit,
                    nominalAmount: record.nominal_amount,
                    validators: record.validators,
                    waa: record.waa,
                    sra: record.sra,
                    cca: record.cca,
                    nods: record.nods,
                    intexes: record.intexes,
                    promisLimit: record.promis_limit,
                })
            }),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{desis_state, emission_day_state, metadosis_state, EmissionDay};
    use alloy_sol_types::SolCall;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use outbe_system_common::time::WorldwideDay;

    #[test]
    fn get_emission_day_returns_all_fields_and_reverts_when_missing() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);

        let missing = dispatch(
            storage.clone(),
            &IEmission::getEmissionDayCall { day: 20261014 }.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap_err();
        assert!(missing.to_string().contains("EmissionDay not found"));

        let record = EmissionDay {
            day: 20261014,
            wwd: WorldwideDay::new(20261013),
            metadosis_start: 1_792_022_400,
            state: emission_day_state::COMPLETED,
            metadosis_state: metadosis_state::COMPLETED,
            desis_state: desis_state::COMPLETED,
            emission_limit: 1_000,
            validators_limit: 40,
            cca_limit: 40,
            waa_limit: 40,
            sra_limit: 40,
            metadosis_limit: 840,
            lysis_limit: 320,
            desis_limit: 900,
            nominal_amount: 1_000,
            validators: 40,
            waa: 39,
            sra: 40,
            cca: 40,
            nods: 320,
            intexes: 700,
            promis_limit: -179,
        };
        EmissionContract::new(storage.clone())
            .emission_days
            .create(&record)
            .unwrap();

        let bytes = dispatch(
            storage,
            &IEmission::getEmissionDayCall { day: 20261014 }.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let decoded = IEmission::getEmissionDayCall::abi_decode_returns(&bytes).unwrap();
        assert_eq!(decoded.wwd, 20261013);
        assert_eq!(decoded.desisState, desis_state::COMPLETED);
        assert_eq!(decoded.validatorsLimit, 40);
        assert_eq!(decoded.metadosisLimit, 840);
        assert_eq!(decoded.lysisLimit, 320);
        assert_eq!(decoded.nominalAmount, 1_000);
        assert_eq!(decoded.waa, 39);
        assert_eq!(decoded.intexes, 700);
        assert_eq!(decoded.promisLimit, -179);
    }
}
