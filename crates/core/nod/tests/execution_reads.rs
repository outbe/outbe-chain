use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_nod::{api, precompile::INod, NodItemState};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::NOD_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

fn item(owner: Address, day: WorldwideDay) -> NodItemState {
    let nod_id = outbe_nod::identity::generate_nod_id(owner, day).unwrap();
    outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id,
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day: day,
            league_id: 4,
            group_key: outbe_nod::test_support::group_key(1_752_534_000, U256::from(5), 978),
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_752_534_000,
        },
        U256::from(5),
    )
}

/// The fixtures key their groups by the production call rate.
fn seed_production_profile(storage: &StorageHandle<'_>) {
    outbe_nod::NodContract::new(storage.clone())
        .config_profile
        .write(outbe_nod::config::PROFILE_PROD)
        .unwrap();
}

fn provider() -> HashMapStorageProvider {
    HashMapStorageProvider::new(1)
}

#[test]
fn same_block_issue_is_visible_to_point_and_list_reads() {
    let mut provider = provider();
    let body = item(Address::repeat_byte(0x21), WorldwideDay::new(20_260_716));

    StorageHandle::enter(&mut provider, |storage| {
        seed_production_profile(&storage);
        outbe_nod::test_support::add(&storage, &body).unwrap();
        assert_eq!(
            api::get_item(&storage, body.nod_id).unwrap().unwrap().owner,
            body.owner
        );
        assert_eq!(
            api::list_by_owner(&storage, body.owner)
                .unwrap()
                .into_iter()
                .map(|item| item.nod_id)
                .collect::<Vec<_>>(),
            [body.nod_id]
        );
        assert_eq!(api::list_all(&storage).unwrap().len(), 1);
    });

    let signatures: Vec<_> = provider
        .get_events(NOD_ADDRESS)
        .iter()
        .map(|event| event.topics()[0])
        .collect();
    assert_eq!(signatures, [INod::Transfer::SIGNATURE_HASH]);
}

#[test]
fn removal_checks_loaded_state() {
    let mut provider = provider();
    let body = item(Address::repeat_byte(0x41), WorldwideDay::new(20_260_716));
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_profile(&storage);
        outbe_nod::test_support::add(&storage, &body).unwrap();
        let loaded_item = api::load_item(&storage, body.nod_id).unwrap().unwrap();
        let group_id = body.group_key;
        let loaded_group = api::load_group(&storage, group_id).unwrap().unwrap();

        api::remove_nod(&storage, loaded_item, loaded_group).unwrap();

        assert!(api::get_item(&storage, body.nod_id).unwrap().is_none());
    });
}
