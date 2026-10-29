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
                    emissionLimit: record.emission_limit,
                    metadosisLimit: record.metadosis_limit,
                    lysisLimit: record.lysis_limit,
                    desisLimit: record.desis_limit,
                    validators: record.validators,
                    waa: record.waa,
                    sra: record.sra,
                    cca: record.cca,
                    nods: record.nods,
                    promisLimit: record.promis_limit,
                })
            }),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{emission_day_state, metadosis_state, EmissionDay};
    use alloy_sol_types::SolCall;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use outbe_primitives::time::WorldwideDay;

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

        let contract = EmissionContract::new(storage.clone());
        contract
            .emission_days
            .create(&EmissionDay {
                day: 20261014,
                wwd: WorldwideDay::new(20260923),
                metadosis_start: 1_792_022_401,
                state: emission_day_state::PROCESSING,
                metadosis_state: metadosis_state::COMPLETED,
                emission_limit: 1_000,
                metadosis_limit: 840,
                lysis_limit: 300,
                desis_limit: 540,
                validators: 40,
                waa: 40,
                sra: 40,
                cca: 40,
                nods: 280,
                promis_limit: 560,
            })
            .unwrap();

        let bytes = dispatch(
            storage,
            &IEmission::getEmissionDayCall { day: 20261014 }.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let decoded = IEmission::getEmissionDayCall::abi_decode_returns(&bytes).unwrap();
        assert_eq!(decoded.day, 20261014);
        assert_eq!(decoded.wwd, 20260923);
        assert_eq!(decoded.metadosisStart, 1_792_022_401);
        assert_eq!(decoded.state, emission_day_state::PROCESSING);
        assert_eq!(decoded.metadosisState, metadosis_state::COMPLETED);
        assert_eq!(decoded.emissionLimit, 1_000);
        assert_eq!(decoded.metadosisLimit, 840);
        assert_eq!(decoded.lysisLimit, 300);
        assert_eq!(decoded.desisLimit, 540);
        assert_eq!(decoded.validators, 40);
        assert_eq!(decoded.waa, 40);
        assert_eq!(decoded.sra, 40);
        assert_eq!(decoded.cca, 40);
        assert_eq!(decoded.nods, 280);
        assert_eq!(decoded.promisLimit, 560);
    }
}
