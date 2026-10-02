use super::*;
use alloy_sol_types::SolCall;

fn query<C: SolCall>(storage: StorageHandle<'_>, call: C) -> C::Return {
    let output =
        metadosis_dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO).unwrap();
    C::abi_decode_returns(&output).unwrap()
}

#[test]
fn missing_receipts_have_canonical_empty_abi_returns() {
    with_storage(|storage| {
        let wwd = 20270101;
        let terminal = query(
            storage.clone(),
            IMetadosis::getWorldwideDayTerminalReceiptCall { wwd },
        );
        assert_eq!(terminal.outcome, 0);
        assert_eq!(terminal.unusedMetadosisLimitMinor, U256::ZERO);
        assert_eq!(terminal.promisLimitBeforeMinor, U256::ZERO);
        assert_eq!(terminal.promisLimitAfterMinor, U256::ZERO);
        assert_eq!(terminal.retirementOutcome, 0);
        assert_eq!(terminal.blockNumber, 0);
        let capacity = query(
            storage.clone(),
            IMetadosis::getCapacityForfeitureReceiptCall { wwd },
        );
        assert_eq!(capacity.outcome, 0);
        assert_eq!(capacity.maxRetainedWorldwideDays, 0);
        assert_eq!(capacity.retainedCountBefore, 0);
        assert_eq!(capacity.unusedMetadosisLimitMinor, U256::ZERO);
        assert_eq!(capacity.sealedCollectionRoot, B256::ZERO);
        assert_eq!(capacity.forfeitedTributeCount, 0);
        assert_eq!(capacity.forfeitedTributeNominalMinor, U256::ZERO);
        assert_eq!(capacity.sourceGeneration, 0);
        assert_eq!(capacity.retiredGeneration, 0);
        assert_eq!(capacity.retirementOutcome, 0);
        assert_eq!(capacity.blockNumber, 0);
    });
}

#[test]
fn missing_ocomp_artifacts_revert_with_their_record_identity() {
    with_storage(|storage| {
        for (call, expected) in [
            (
                IMetadosis::getOffchainJobCall {
                    intentId: B256::repeat_byte(1),
                }
                .abi_encode(),
                "OcompJobRecordV1 not found",
            ),
            (
                IMetadosis::getOffchainVoteAccountabilityCall {
                    jobId: B256::repeat_byte(2),
                }
                .abi_encode(),
                "OcompVoteAccountabilityV1 not found",
            ),
            (
                IMetadosis::getActiveLysisGenerationCall { wwd: 20270101 }.abi_encode(),
                "ActiveGenerationV1 not found",
            ),
            (
                IMetadosis::getLysisTerminalReceiptCall {
                    intentId: B256::repeat_byte(1),
                }
                .abi_encode(),
                "OcompJobRecordV1 not found",
            ),
        ] {
            let error =
                metadosis_dispatch(storage.clone(), &call, Address::ZERO, U256::ZERO).unwrap_err();
            assert!(
                matches!(error, outbe_primitives::error::PrecompileError::Revert(message) if message == expected)
            );
        }
    });
}

#[cfg(feature = "test-utils")]
#[test]
fn worldwide_day_abi_preserves_genesis_windows_rates_and_membership() {
    with_storage(|storage| {
        seed_offering_day(storage.clone());
        let day = query(
            storage.clone(),
            IMetadosis::getWorldwideDayCall { wwd: 20270101 },
        );
        assert_eq!(day.status, status::OFFERING);
        assert_eq!(day.dayType, day_type::GREEN);
        assert_eq!(
            (
                day.formingStart,
                day.formingEnd,
                day.lookbackEnd,
                day.offeringEnd,
                day.scheduledProcessTime
            ),
            (10, 20, 30, 40, 50)
        );
        assert_eq!(
            (day.previousVwapMinor, day.currentVwapMinor),
            (U256::from(8), U256::from(9))
        );
        let active: Vec<u32> = query(storage.clone(), IMetadosis::getActiveWorldwideDaysCall {});
        assert_eq!(active, vec![20270101]);
        let offering: Vec<u32> = query(
            storage.clone(),
            IMetadosis::getWorldwideDaysByStatusCall {
                status: status::OFFERING,
            },
        );
        assert_eq!(offering, vec![20270101]);
        MetadosisContract::new(storage.clone())
            .bootstrap_end_time
            .write(123)
            .unwrap();
        let bootstrap: u64 = query(storage, IMetadosis::getBootstrapEndTimeCall {});
        assert_eq!(bootstrap, 123);
    });
}

#[cfg(feature = "test-utils")]
pub(super) fn seed_offering_day(storage: StorageHandle<'_>) {
    use crate::genesis::{FreshDevnetGenesisBuilder, GenesisWorldwideDay};
    FreshDevnetGenesisBuilder::new()
        .seed_active_worldwide_day(GenesisWorldwideDay {
            worldwide_day: 20270101.into(),
            status: crate::WwdStatus::Offering,
            day_type: crate::WwdDayType::Green,
            forming_start: 10,
            forming_end: 20,
            lookback_end: 30,
            offering_end: 40,
            scheduled_process_time: 50,
            metadosis_limit_amount: U256::from(100),
            previous_vwap: U256::from(8),
            current_vwap: U256::from(9),
        })
        .apply(storage.clone())
        .unwrap();
}
