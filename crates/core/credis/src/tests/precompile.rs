use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::time::SECONDS_PER_DAY;

use crate::constants::{CALL_NOTICE_PERIOD, CALL_THRESHOLD, CALL_WINDOW, TOKEN_NAME};
use crate::precompile::{dispatch, ICredis};
use crate::schema::{Credis, CredisContract, CredisState};

const ISSUED_AT: u64 = 1_700_000_000;

fn fixture(storage: StorageHandle<'_>) -> Credis {
    let record = Credis {
        credis_id: U256::ONE,
        owner: Address::repeat_byte(1),
        cca: Address::repeat_byte(2),
        asset: Address::repeat_byte(3),
        issuance_currency: 840,
        source: Address::repeat_byte(4),
        principal_minor: U256::from(365_000),
        outstanding_principal_minor: U256::from(365_000),
        gratis_minor: U256::from(100),
        outstanding_gratis_minor: U256::from(100),
        policy_rate: U256::from(1_000_000),
        entry_price_minor: U256::from(1_000_000),
        call_price_minor: U256::from(1_640_000),
        issued_at: ISSUED_AT,
        last_settled_at: ISSUED_AT,
        called_at: 0,
        state: CredisState::Issued as u8,
        reference_currency: 840,
        call_notice_period_seconds: CALL_NOTICE_PERIOD,
        call_rate: 64,
        call_window_seconds: CALL_WINDOW,
        call_threshold_seconds: CALL_THRESHOLD,
        call_anchor_price_minor: U256::from(1_000_000),
        interest_paid_minor: U256::ZERO,
    };
    let mut contract = CredisContract::new(storage);
    contract.create_credis_record(&record).unwrap();
    contract
        .record_group_called(record.group_key(), ISSUED_AT + SECONDS_PER_DAY)
        .unwrap();
    contract.get_credis(record.credis_id).unwrap()
}

fn call<T: SolCall>(storage: &StorageHandle<'_>, call: T) -> Result<Bytes> {
    dispatch(
        storage.clone(),
        &call.abi_encode(),
        Address::ZERO,
        U256::ZERO,
    )
}

fn timed_calls(id: U256) -> [Vec<u8>; 3] {
    [
        ICredis::tokenURICall { credisId: id }.abi_encode(),
        ICredis::getCredisCall { credisId: id }.abi_encode(),
        ICredis::interestAccruedMinorCall { credisId: id }.abi_encode(),
    ]
}

#[test]
fn timestamp_overflow_reverts_on_all_time_dependent_views() {
    for timestamp in [U256::from(u64::MAX) + U256::ONE, U256::MAX] {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_timestamp(timestamp);
        provider.enter(|storage| {
            let record = fixture(storage.clone());
            for data in timed_calls(record.credis_id) {
                let result = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO);
                assert!(matches!(result, Err(PrecompileError::Revert(ref reason))
                    if reason == "credis timestamp exceeds u64"));
            }
        });
    }
}

#[test]
fn valid_timestamps_preserve_view_results_including_u64_max() {
    for now in [0, ISSUED_AT + 2 * SECONDS_PER_DAY, u64::MAX] {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_timestamp(U256::from(now));
        provider.enter(|storage| {
            let record = fixture(storage.clone());
            let id = record.credis_id;
            let forfeited = now > record.called_at + u64::from(CALL_NOTICE_PERIOD);
            let out = call(&storage, ICredis::getCredisCall { credisId: id }).unwrap();
            let decoded = ICredis::getCredisCall::abi_decode_returns(&out).unwrap();
            assert_eq!(decoded.credisId, id);
            assert_eq!(decoded.owner, record.owner);
            assert_eq!(decoded.principalMinor, record.principal_minor);
            assert_eq!(decoded.gratisMinor, record.gratis_minor);
            assert_eq!(decoded.call.calledAt, record.called_at);
            assert_eq!(
                decoded.call.settlementDeadline,
                record.called_at + u64::from(CALL_NOTICE_PERIOD)
            );
            assert_eq!(
                decoded.state,
                if forfeited {
                    CredisState::Forfeited
                } else {
                    CredisState::Called
                } as u8
            );
            assert_eq!(
                decoded.outstandingPrincipalMinor,
                if forfeited {
                    U256::ZERO
                } else {
                    record.principal_minor
                }
            );
            assert_eq!(
                decoded.outcome.principalWrittenOffMinor,
                if forfeited {
                    record.principal_minor
                } else {
                    U256::ZERO
                }
            );
            assert_eq!(
                decoded.outstandingGratisMinor,
                if forfeited {
                    U256::ZERO
                } else {
                    record.gratis_minor
                }
            );
            assert_eq!(
                decoded.outcome.gratisBurnedMinor,
                if forfeited {
                    record.gratis_minor
                } else {
                    U256::ZERO
                }
            );

            let out = call(&storage, ICredis::interestAccruedMinorCall { credisId: id }).unwrap();
            // 365,000 principal at 100% ACT/365 accrues 1,000 per whole day.
            let expected = if forfeited {
                0
            } else {
                now.saturating_sub(ISSUED_AT) / SECONDS_PER_DAY * 1_000
            };
            assert_eq!(
                ICredis::interestAccruedMinorCall::abi_decode_returns(&out).unwrap(),
                U256::from(expected)
            );

            let out = call(&storage, ICredis::tokenURICall { credisId: id }).unwrap();
            assert_eq!(
                ICredis::tokenURICall::abi_decode_returns(&out).unwrap(),
                crate::metadata::token_uri(&record, now).unwrap()
            );
        });
    }
}

#[test]
fn overflowing_time_preserves_missing_record_errors_and_time_independent_views() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::MAX);
    provider.enter(|storage| {
        for data in timed_calls(U256::ONE) {
            let result = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO);
            assert!(matches!(result, Err(PrecompileError::Revert(ref reason))
                if reason == "Credis not found"));
        }
        let out = call(&storage, ICredis::nameCall {}).unwrap();
        assert_eq!(
            ICredis::nameCall::abi_decode_returns(&out).unwrap(),
            TOKEN_NAME
        );
    });
}
