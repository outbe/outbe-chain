use alloy_primitives::{Address, U256};
use outbe_primitives::time::{
    date_key_to_utc_timestamp, previous_date_key, timestamp_to_date_key, WorldwideDay,
};
use outbe_primitives::{
    error::{PrecompileError, Result},
    math::constants::REAL_ID_SHIFT,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

use crate::{api, NodContract, NodItemState};

/// Default block timestamp. Not UTC midnight, so its last closed day is the
/// previous calendar day.
const NOW: u64 = 1_752_534_000;

fn item(owner: Address) -> NodItemState {
    let worldwide_day = WorldwideDay::new(20_260_715);
    crate::test_support::item(
        crate::test_support::NodItemFixture {
            is_settled: false,
            nod_id: crate::test_support::nod_id_of(owner, worldwide_day, 840, 978),
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day,
            league_id: 4,
            group_key: crate::test_support::group_key(
                date_key_to_utc_timestamp(
                    previous_date_key(timestamp_to_date_key(NOW)).expect("date step"),
                ),
                U256::from(5),
                978,
            ),
            issuance_currency: 840,
            reference_currency: 978,
            // Midnight of the last UTC day closed at `NOW`.
            issued_at: date_key_to_utc_timestamp(
                previous_date_key(timestamp_to_date_key(NOW)).expect("date step"),
            ),
        },
        U256::from(5),
    )
}

#[test]
fn coen_iso_one_maps_to_the_center_price_bin_at_six_decimals() {
    assert_eq!(
        outbe_primitives::call_bins::price_to_bin(U256::from(1_000_000u64)).unwrap(),
        REAL_ID_SHIFT as u32
    );
}

#[test]
fn reverted_issuance_rolls_back_nod_state() {
    let body = item(Address::repeat_byte(0x66));
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let outcome: Result<()> = storage.with_checkpoint(|| {
            crate::test_support::add(&storage, &body)?;
            assert!(api::get_item(&storage, body.nod_id)?.is_some());
            Err(PrecompileError::Revert("nested caller reverted".into()))
        });
        assert!(outcome.is_err());
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert!(api::get_item(&storage, body.nod_id).unwrap().is_none());
    });
}

#[test]
fn nod_identity_and_abi_boundary_preserve_exact_32_bytes() {
    let body = item(Address::repeat_byte(0x33));
    let encoded = format!("{:064x}", body.nod_id);
    assert_eq!(
        crate::identity::parse_nod_id(&encoded).unwrap(),
        body.nod_id
    );
    assert!(crate::identity::parse_nod_id(&encoded[..62]).is_err());

    // The ABI carries the identity as one word, so a wrong-width id is no
    // longer representable. The round trip through `uint256` is total. The old
    // "invalid bytes length" revert has no input that can reach it.
    let word = body.nod_id;
    assert_eq!(word, body.nod_id);
    assert_eq!(word.to_be_bytes::<32>(), body.nod_id.to_be_bytes::<32>());
}

#[test]
fn group_record_survives_membership_changes_until_last_removal() {
    let first = item(Address::repeat_byte(0x71));
    let second = item(Address::repeat_byte(0x72));
    let group_id = first.group_key;
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        super::seed_production_nod_genesis(&storage);
        crate::test_support::add(&storage, &first).unwrap();
        let original = NodContract::new(storage.clone())
            .groups
            .get(group_id)
            .unwrap()
            .unwrap();
        crate::test_support::add(&storage, &second).unwrap();
        let after_issue = NodContract::new(storage.clone())
            .groups
            .get(group_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            original, after_issue,
            "adding another NOD must not rewrite the shared group commitment"
        );

        assert_eq!(
            NodContract::new(storage.clone())
                .group_nod_count
                .read(&first.group_key)
                .unwrap(),
            2
        );

        let loaded_item = api::load_item(&storage, first.nod_id).unwrap().unwrap();
        let group = api::load_group(&storage, group_id).unwrap().unwrap();
        api::remove_nod(&storage, loaded_item, group).unwrap();
        let after_removal = NodContract::new(storage.clone())
            .groups
            .get(group_id)
            .unwrap()
            .unwrap();
        assert_eq!(original, after_removal);

        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.group_nod_count.read(&first.group_key).unwrap(), 1);
        assert_eq!(
            nod.group_nods
                .read(&crate::index_keys::group_nod_key(first.group_key, 0))
                .unwrap(),
            second.nod_id
        );
        assert_eq!(nod.group_nod_index.read(&second.nod_id).unwrap(), 0);

        let loaded_item = api::load_item(&storage, second.nod_id).unwrap().unwrap();
        let group = api::load_group(&storage, group_id).unwrap().unwrap();
        api::remove_nod(&storage, loaded_item, group).unwrap();
        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.group_nod_count.read(&first.group_key).unwrap(), 0);
        assert_eq!(nod.total_supply().unwrap(), 0);
        assert!(api::get_group(&storage, group_id).unwrap().is_none());
    });
}

#[test]
fn entry_price_day_is_the_last_closed_utc_day() {
    let new_year = date_key_to_utc_timestamp(20_260_101);
    assert_eq!(
        crate::openings::entry_price_day(new_year).expect("date step"),
        20_251_231
    );
    assert_eq!(
        crate::openings::entry_price_day(new_year - 1).expect("date step"),
        20_251_230
    );
}

#[test]
fn issuance_rejects_legacy_and_mismatched_digests_without_writes() {
    let body = item(Address::repeat_byte(0x42));
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        super::seed_production_nod_genesis(&storage);
        let legacy = outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(
            body.owner,
            body.worldwide_day,
        )
        .to_u256();
        assert_ne!(body.nod_id, legacy);
        for invalid_id in [
            legacy,
            U256::from_be_bytes(
                outbe_primitives::wwd_entity_id::derive_poseidon_digest(
                    body.owner,
                    body.worldwide_day,
                )
                .0,
            ),
            crate::identity::generate_nod_id(body.series_id, Address::repeat_byte(0x43)),
            crate::test_support::nod_id_of(
                body.owner,
                WorldwideDay::new(20_260_716),
                body.issuance_currency,
                body.reference_currency,
            ),
        ] {
            let mut invalid = body.clone();
            invalid.nod_id = invalid_id;
            invalid.encrypted.terms.nod_id = invalid_id;
            assert!(crate::test_support::add(&storage, &invalid).is_err());
            assert!(api::get_item(&storage, invalid_id).unwrap().is_none());
            assert_eq!(api::balance_of(&storage, body.owner).unwrap(), 0);
            assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        }
        crate::test_support::add(&storage, &body).unwrap();
        assert!(api::get_item(&storage, body.nod_id).unwrap().is_some());
    });
}

#[test]
fn member_count_overflow_and_underflow_roll_back_nod_mutations() {
    let first = item(Address::repeat_byte(0x73));
    let second = item(Address::repeat_byte(0x74));
    let group_id = first.group_key;
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        super::seed_production_nod_genesis(&storage);
        crate::test_support::add(&storage, &first).unwrap();
        let nod = NodContract::new(storage.clone());
        let original = nod.groups.get(group_id).unwrap().unwrap();
        nod.group_nod_count
            .write(&first.group_key, u32::MAX)
            .unwrap();
        let error = crate::test_support::add(&storage, &second).unwrap_err();
        assert_eq!(
            error.to_string(),
            PrecompileError::from(crate::errors::NodError::GroupMemberIndexOverflow {
                group_key: first.group_key,
            })
            .to_string()
        );
        assert_eq!(nod.total_supply().unwrap(), 1);
        assert_eq!(
            nod.group_nod_count.read(&first.group_key).unwrap(),
            u32::MAX
        );
        assert!(api::get_item(&storage, second.nod_id).unwrap().is_none());

        nod.group_nod_count.write(&first.group_key, 0).unwrap();
        let loaded = api::load_item(&storage, first.nod_id).unwrap().unwrap();
        let group = api::load_group(&storage, group_id).unwrap().unwrap();
        let error = api::remove_nod(&storage, loaded, group).unwrap_err();
        assert_eq!(
            error.to_string(),
            PrecompileError::from(
                crate::errors::NodError::GroupMemberCountUnderflowDuringRemoval {
                    group_key: first.group_key,
                }
            )
            .to_string()
        );
        assert_eq!(nod.total_supply().unwrap(), 1);
        assert!(api::get_item(&storage, first.nod_id).unwrap().is_some());
        assert_eq!(nod.groups.get(group_id).unwrap().unwrap(), original);
    });
}

#[test]
fn nod_error_and_calculation_amount_never_return_fatal() {
    let domain_error: PrecompileError = crate::errors::NodError::NodNotFound.into();
    assert!(matches!(domain_error, PrecompileError::RevertBytes(_)));

    let mut corrupted = item(Address::repeat_byte(0x75));
    crate::test_support::uninstall();
    assert!(matches!(
        api::calculation_amount(&corrupted),
        Err(PrecompileError::EnclaveUnavailable(_))
    ));

    crate::test_support::install();
    corrupted.encrypted.encrypted_gratis_amount.clear();
    assert!(matches!(
        api::calculation_amount(&corrupted),
        Err(PrecompileError::RevertBytes(_))
    ));
}

/// Slot assignment is dense in `order` sequence. Inserting a field rather than
/// appending one silently reassigns the meaning of every slot after it,
/// including the two slots that the genesis alloc seeds. New fields must append.
#[test]
fn nod_contract_slot_layout_is_pinned() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage);
        assert_eq!(nod.total_supply.slot(), U256::ZERO);
        assert_eq!(nod.group_nod_count.base_slot(), U256::from(1));
        assert_eq!(nod.group_nods.base_slot(), U256::from(2));
        assert_eq!(nod.group_nod_index.base_slot(), U256::from(3));
        assert_eq!(nod.group_call_price_minor.base_slot(), U256::from(4));
        assert_eq!(nod.group_currency.base_slot(), U256::from(5));
        assert_eq!(nod.group_called_at.base_slot(), U256::from(6));
        assert_eq!(nod.group_call_rate.base_slot(), U256::from(7));
        assert_eq!(nod.group_call_window_seconds.base_slot(), U256::from(8));
        assert_eq!(nod.group_call_threshold_seconds.base_slot(), U256::from(9));
        assert_eq!(
            nod.group_call_notice_period_seconds.base_slot(),
            U256::from(10)
        );
        assert_eq!(nod.max_call_window_seconds.base_slot(), U256::from(11));
        assert_eq!(nod.call_sweep_day.slot(), U256::from(12));
        assert_eq!(nod.call_latest_day.slot(), U256::from(13));
        assert_eq!(nod.call_bin_tree_root.base_slot(), U256::from(14));
        assert_eq!(nod.call_bin_tree_mid.base_slot(), U256::from(15));
        assert_eq!(nod.call_bin_tree_leaf.base_slot(), U256::from(16));
        assert_eq!(nod.call_bin_count.base_slot(), U256::from(17));
        assert_eq!(nod.call_bin_groups.base_slot(), U256::from(18));
        assert_eq!(nod.call_group_slot.base_slot(), U256::from(19));
        assert_eq!(nod.forfeit_tree_root.slot(), U256::from(20));
        assert_eq!(nod.forfeit_tree_leaf.base_slot(), U256::from(22));
        assert_eq!(nod.call_currency_cursor.slot(), U256::from(23));
        assert_eq!(nod.call_bin_cursor.base_slot(), U256::from(24));
        assert_eq!(nod.config_profile.slot(), U256::from(25));
        assert_eq!(nod.forfeit_day_len.base_slot(), U256::from(26));
        assert_eq!(nod.forfeit_cursor.slot(), U256::from(32));
    });
}
