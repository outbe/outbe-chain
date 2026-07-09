use alloy_primitives::{address, keccak256, Address, U256};
use alloy_sol_types::{SolCall, SolInterface};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::{dispatch, ICredis};
use crate::schema::{CredisContract, NUMBER_OF_ANADOSIS, SECONDS_PER_MONTH};

const CHAIN_ID: u64 = 1;
const CREATED_AT: u64 = 1_700_000_000;

fn alice() -> Address {
    address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
}

fn bob() -> Address {
    address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB")
}

fn asset() -> Address {
    address!("0x0000000000000000000000000000000000000888")
}

fn test_commitment() -> U256 {
    U256::from_be_bytes(keccak256([0x33, 0x01]).0)
}

fn other_commitment() -> U256 {
    U256::from_be_bytes(keccak256([0x33, 0x02]).0)
}

fn due_date_for(anadosis_number: u32) -> u64 {
    CREATED_AT + (anadosis_number as u64) * SECONDS_PER_MONTH
}

fn with_credis<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, f)
}

// ------------------------------------------------------------------
// Position key derivation matches cosmos exactly
// ------------------------------------------------------------------

#[test]
fn position_id_matches_keccak() {
    let id = CredisContract::position_id(test_commitment(), alice());
    let mut buf = [0u8; 52];
    buf[0..32].copy_from_slice(&test_commitment().to_be_bytes::<32>());
    buf[32..52].copy_from_slice(alice().as_slice());
    assert_eq!(id, U256::from_be_bytes(keccak256(buf).0));
}

#[test]
fn anadosis_key_matches_layout() {
    let id = CredisContract::position_id(test_commitment(), alice());
    let key = CredisContract::anadosis_key(id, 7);
    let mut buf = [0u8; 36];
    buf[0..32].copy_from_slice(&id.to_be_bytes::<32>());
    buf[32..36].copy_from_slice(&7u32.to_be_bytes());
    assert_eq!(key, keccak256(buf));
}

// ------------------------------------------------------------------
// create_position
// ------------------------------------------------------------------

#[test]
fn create_position_populates_all_10_anadosis_records() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        let position_id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(100_000u64),
                U256::from(50_000u64),
                CREATED_AT,
            )
            .unwrap();

        let position = credis.get_position(position_id).unwrap();
        assert_eq!(position.bundle_account, alice());
        assert_eq!(position.asset, asset());
        assert_eq!(position.total_anadosis_amount, U256::from(100_000u64));
        assert_eq!(position.outstanding_anadosis_amount, U256::from(100_000u64));
        assert_eq!(position.total_gratis_amount, U256::from(50_000u64));
        assert_eq!(position.outstanding_gratis_amount, U256::from(50_000u64));
        assert_eq!(position.next_anadosis_number, 1);
        assert_eq!(position.created_at, CREATED_AT);

        for n in 1..=NUMBER_OF_ANADOSIS {
            let anadosis = credis.get_anadosis(position_id, n).unwrap();
            assert_eq!(anadosis.anadosis_number, n);
            assert_eq!(anadosis.due_date, due_date_for(n));
            assert_eq!(anadosis.paid_at, 0);
        }
    });
}

#[test]
fn create_position_applies_refinancing_rate_to_total_debt() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        // BRD example: principal 1000, refinancing rate 4.5% (45e15), term 10
        // months -> total_debt = 1000 * (1 + 0.045 * 10/12) = 1037.5 -> 1037.
        let principal = U256::from(1000u64);
        let rate = U256::from(45_000_000_000_000_000u128); // 0.045 @ 1e18
        let position_id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                rate,
                principal,
                U256::from(500u64),
                CREATED_AT,
            )
            .unwrap();

        let position = credis.get_position(position_id).unwrap();
        assert_eq!(position.credis_principal, principal);
        assert_eq!(position.refinancing_rate, rate);
        assert_eq!(position.issuance_currency, 840);
        assert_eq!(position.total_anadosis_amount, U256::from(1037u64));
        assert_eq!(position.outstanding_anadosis_amount, U256::from(1037u64));

        // The 10 installments sum exactly to the total debt.
        let mut sum = U256::ZERO;
        for n in 1..=NUMBER_OF_ANADOSIS {
            sum += credis.get_anadosis(position_id, n).unwrap().anadosis_amount;
        }
        assert_eq!(sum, U256::from(1037u64));
    });
}

#[test]
fn create_position_zero_rate_matches_principal() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        let principal = U256::from(1_234u64);
        let position_id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                principal,
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();
        let position = credis.get_position(position_id).unwrap();
        assert_eq!(position.total_anadosis_amount, principal);
        assert_eq!(position.credis_principal, principal);
    });
}

#[test]
fn create_position_rejects_duplicate() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(1000u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();
        let err = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(1000u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    });
}

#[test]
fn create_position_rejects_zero_amount() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        let err = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::ZERO,
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap_err();
        assert!(err.to_string().contains("positive"));
    });
}

#[test]
fn create_position_grows_address_index() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(1000u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();
        credis
            .create_position(
                other_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(2000u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();

        let positions = credis.get_positions_by_address(alice()).unwrap();
        assert_eq!(positions.len(), 2);
        assert_eq!(positions[0].total_anadosis_amount, U256::from(1000u64));
        assert_eq!(positions[1].total_anadosis_amount, U256::from(2000u64));

        let all = credis.get_all_positions().unwrap();
        assert_eq!(all.len(), 2);
    });
}

// ------------------------------------------------------------------
// remainder absorption (sum == total exactly)
// ------------------------------------------------------------------

#[test]
fn anadosis_amount_equal_split_without_remainder() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        // 100 / 10 = 10 exact.
        let id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(100u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();
        for n in 1..=NUMBER_OF_ANADOSIS {
            assert_eq!(
                credis.get_anadosis(id, n).unwrap().anadosis_amount,
                U256::from(10u64),
            );
        }
    });
}

#[test]
fn anadosis_amount_remainder_absorbed_in_last_anadosis() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        // 105 / 10 = 10 remainder 5 → anadosis 1..9 = 10, anadosis 10 = 15.
        let id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(105u64),
                U256::from(53u64), // gratis 53 / 10 = 5 r3 → anadosis 1..9 = 5, anadosis 10 = 8
                CREATED_AT,
            )
            .unwrap();

        for n in 1..NUMBER_OF_ANADOSIS {
            assert_eq!(
                credis.get_anadosis(id, n).unwrap().anadosis_amount,
                U256::from(10u64),
            );
            assert_eq!(
                credis.get_anadosis(id, n).unwrap().gratis_amount,
                U256::from(5u64),
            );
        }
        let last = credis.get_anadosis(id, NUMBER_OF_ANADOSIS).unwrap();
        assert_eq!(last.anadosis_amount, U256::from(15u64));
        assert_eq!(last.gratis_amount, U256::from(8u64));

        let mut a_sum = U256::ZERO;
        let mut g_sum = U256::ZERO;
        for n in 1..=NUMBER_OF_ANADOSIS {
            let a = credis.get_anadosis(id, n).unwrap();
            a_sum += a.anadosis_amount;
            g_sum += a.gratis_amount;
        }
        assert_eq!(a_sum, U256::from(105u64), "sum(anadosis_amount) == total");
        assert_eq!(g_sum, U256::from(53u64), "sum(gratis_amount) == total");
    });
}

// ------------------------------------------------------------------
// make_next_anadosis: sequential, completed
// ------------------------------------------------------------------

#[test]
fn make_next_anadosis_advances_pointer() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        let id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(100_000u64),
                U256::from(50_000u64),
                CREATED_AT,
            )
            .unwrap();

        for n in 1..=NUMBER_OF_ANADOSIS {
            let result = credis.make_next_anadosis(id, due_date_for(n)).unwrap();
            assert_eq!(result.anadosis_number, n);
            assert_eq!(result.bundle_account, alice());
            assert_eq!(result.asset, asset());
            assert_eq!(result.paid_at, due_date_for(n));
        }
        // After the 10th anadosis, position is completed.
        let err = credis
            .make_next_anadosis(id, due_date_for(NUMBER_OF_ANADOSIS) + 1)
            .unwrap_err();
        assert!(err.to_string().contains("completed"));
    });
}

#[test]
fn make_next_anadosis_decrements_outstanding() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        let id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(100u64),
                U256::from(50u64),
                CREATED_AT,
            )
            .unwrap();

        credis.make_next_anadosis(id, due_date_for(1)).unwrap();
        let p = credis.get_position(id).unwrap();
        assert_eq!(p.outstanding_anadosis_amount, U256::from(90u64));
        assert_eq!(p.outstanding_gratis_amount, U256::from(45u64));
        assert_eq!(p.next_anadosis_number, 2);
    });
}

#[test]
fn make_next_anadosis_accepted_before_due_date() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        let id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(1_000u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();
        credis.make_next_anadosis(id, due_date_for(1) - 1).unwrap();

        // State unchanged.
        let p = credis.get_position(id).unwrap();
        assert_eq!(p.next_anadosis_number, 2);
        assert_eq!(p.outstanding_anadosis_amount, U256::from(900u64));
    });
}

#[test]
fn make_next_anadosis_accepted_at_and_after_due_date() {
    // boundary: == is accepted; > is accepted.
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        let id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(1_000u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();
        credis.make_next_anadosis(id, due_date_for(1)).unwrap();

        let id2 = credis
            .create_position(
                other_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(1_000u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();
        credis
            .make_next_anadosis(id2, due_date_for(1) + 7 * 24 * 3600)
            .unwrap();
    });
}

#[test]
fn make_next_anadosis_rejects_missing_position() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        let unknown = U256::from_be_bytes(keccak256([0xff]).0);
        let err = credis
            .make_next_anadosis(unknown, due_date_for(1))
            .unwrap_err();
        assert!(err.to_string().contains("position not found"));
    });
}

// ------------------------------------------------------------------
// read validations
// ------------------------------------------------------------------

#[test]
fn get_anadosis_rejects_anadosis_number_zero() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        let id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(1_000u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();
        let err = credis.get_anadosis(id, 0).unwrap_err();
        assert!(err.to_string().contains("invalid anadosis"));
    });
}

#[test]
fn get_anadosis_rejects_anadosis_number_above_cap() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        let id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(1_000u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();
        let err = credis.get_anadosis(id, NUMBER_OF_ANADOSIS + 1).unwrap_err();
        assert!(err.to_string().contains("invalid anadosis"));
    });
}

#[test]
fn get_anadosis_rejects_missing_position() {
    with_credis(|storage| {
        let credis = CredisContract::new(storage.clone());
        let unknown = U256::from_be_bytes(keccak256([0xfe]).0);
        let err = credis.get_anadosis(unknown, 1).unwrap_err();
        assert!(err.to_string().contains("position not found"));
    });
}

// ------------------------------------------------------------------
// has_overdue_anadosis / get_outstanding_amount
// ------------------------------------------------------------------

#[test]
fn has_overdue_anadosis_reflects_past_due_unpaid_anadosis() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        let id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(100u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();

        assert!(!credis.has_overdue_anadosis(alice(), CREATED_AT).unwrap());
        // Past due_date for anadosis 1 with paid_at == 0 → overdue.
        assert!(credis
            .has_overdue_anadosis(alice(), due_date_for(1) + 1)
            .unwrap());
        // Pay it; now not overdue.
        credis.make_next_anadosis(id, due_date_for(1) + 1).unwrap();
        assert!(!credis
            .has_overdue_anadosis(alice(), due_date_for(1) + 1)
            .unwrap());
    });
}

#[test]
fn get_outstanding_amount_sums_across_positions() {
    with_credis(|storage| {
        let mut credis = CredisContract::new(storage.clone());
        credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(100u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();
        credis
            .create_position(
                other_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(50u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();

        assert_eq!(
            credis.get_outstanding_amount(alice()).unwrap(),
            U256::from(150u64),
        );
        assert_eq!(credis.get_outstanding_amount(bob()).unwrap(), U256::ZERO,);
    });
}

// ------------------------------------------------------------------
// Precompile dispatch (read-only ABI surface)
// ------------------------------------------------------------------

#[test]
fn precompile_get_position_returns_full_record() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let mut credis = CredisContract::new(storage.clone());
        let id = credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(100_000u64),
                U256::from(50_000u64),
                CREATED_AT,
            )
            .unwrap();

        let call = ICredis::ICredisCalls::getPosition(ICredis::getPositionCall { positionId: id })
            .abi_encode();
        let out = dispatch(storage.clone(), &call, Address::ZERO, U256::ZERO).unwrap();
        let position = ICredis::getPositionCall::abi_decode_returns(&out).unwrap();

        assert_eq!(position.positionId, id);
        assert_eq!(position.bundleAccount, alice());
        assert_eq!(position.totalAnadosisAmount, U256::from(100_000u64));
        assert_eq!(position.nextAnadosisNumber, 1);
        assert_eq!(position.createdAt, CREATED_AT);
    });
}

#[test]
fn precompile_rejects_msg_value() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = ICredis::ICredisCalls::credisOf(ICredis::credisOfCall {
            bundleAccount: alice(),
        })
        .abi_encode();
        let err = dispatch(storage, &call, alice(), U256::from(1u64)).unwrap_err();
        assert!(err.to_string().contains("non-payable"));
    });
}

#[test]
fn precompile_supports_erc165() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = ICredis::ICredisCalls::supportsInterface(ICredis::supportsInterfaceCall {
            interfaceId: alloy_primitives::FixedBytes(ERC165_INTERFACE_ID),
        })
        .abi_encode();
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        assert!(ICredis::supportsInterfaceCall::abi_decode_returns(&out).unwrap());

        let call = ICredis::ICredisCalls::supportsInterface(ICredis::supportsInterfaceCall {
            interfaceId: alloy_primitives::FixedBytes([0xde, 0xad, 0xbe, 0xef]),
        })
        .abi_encode();
        let out = dispatch(storage, &call, alice(), U256::ZERO).unwrap();
        assert!(!ICredis::supportsInterfaceCall::abi_decode_returns(&out).unwrap());
    });
}

#[test]
fn precompile_has_overdue_uses_storage_timestamp() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let mut credis = CredisContract::new(storage.clone());
        credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(100u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();

        let call = ICredis::ICredisCalls::hasOverdueAnadosis(ICredis::hasOverdueAnadosisCall {
            bundleAccount: alice(),
        })
        .abi_encode();
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        let has = ICredis::hasOverdueAnadosisCall::abi_decode_returns(&out).unwrap();
        assert!(!has);
    });

    // Now move past anadosis-1's due_date and re-query.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(due_date_for(1) + 1));
    StorageHandle::enter(&mut storage, |storage| {
        // Re-seed position (different storage instance).
        let mut credis = CredisContract::new(storage.clone());
        credis
            .create_position(
                test_commitment(),
                alice(),
                asset(),
                840,
                U256::ZERO,
                U256::from(100u64),
                U256::ZERO,
                CREATED_AT,
            )
            .unwrap();

        let call = ICredis::ICredisCalls::hasOverdueAnadosis(ICredis::hasOverdueAnadosisCall {
            bundleAccount: alice(),
        })
        .abi_encode();
        let out = dispatch(storage, &call, alice(), U256::ZERO).unwrap();
        let has = ICredis::hasOverdueAnadosisCall::abi_decode_returns(&out).unwrap();
        assert!(has);
    });
}
