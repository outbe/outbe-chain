//! The Qualified view is derived at read time from the gem's own floor and its
//! reference currency's finalized days; nothing about it is stored.

use std::sync::Arc;

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_compressed_entities::test_support::seed_compressed_entities_genesis;
use outbe_compressed_entities::{begin_block, ExecutionReaders, ExecutionScope};
use outbe_gem::{
    api,
    precompile::{dispatch, IGem},
    schema::{GemAddParams, GemContract, GemState},
    GemRepositoryReader,
};
use outbe_offchain_storage::MemoryStorage;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    address_pair::AddressPair,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::{first_full_day, next_date_key, timestamp_to_date_key},
};

const ISSUED: u64 = 1_700_000_000;
const FLOOR: u64 = 540_000;

struct World {
    scope: ExecutionScope,
    parent: GemRepositoryReader,
}

impl World {
    fn readers(&self) -> ExecutionReaders<'_, '_, GemRepositoryReader> {
        ExecutionReaders {
            scope: &self.scope,
            parent: &self.parent,
        }
    }
}

fn provider() -> (HashMapStorageProvider, World) {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(ISSUED));
    let world = World {
        scope: ExecutionScope::default(),
        parent: outbe_gem::gem_reader(Arc::new(MemoryStorage::new())),
    };
    StorageHandle::enter(&mut provider, |s| {
        seed_compressed_entities_genesis(&s).unwrap();
        begin_block(s, &world.scope).unwrap();
    });
    (provider, world)
}

fn issue(s: &StorageHandle<'_>, world: &World) -> U256 {
    api::add_gem(
        s,
        &world.scope,
        &world.parent,
        GemAddParams {
            owner: Address::repeat_byte(0x11),
            gem_type: 2,
            promis_load_minor: U256::from(1_000_000),
            entry_price_minor: U256::from(500_000),
            floor_price_minor: U256::from(FLOOR),
            issuance_currency: 978,
            reference_currency: 840,
            issued_at: ISSUED,
        },
    )
    .unwrap()
}

fn close(s: &StorageHandle<'_>, day: u32, iso: u16, price: u64) {
    let oracle = OracleContract::new(s.clone());
    let pair = oracle.pair_index_of(AddressPair::new_coen_to(iso)).unwrap();
    let pair = if pair == 0 {
        outbe_oracle::api::register_pair(s.clone(), AddressPair::new_coen_to(iso)).unwrap()
    } else {
        pair
    };
    oracle
        .record_utc_day_vwap(day, pair, U256::from(price))
        .unwrap();
    oracle.utc_day_vwap_last_finalized.write(day).unwrap();
}

fn status(s: &StorageHandle<'_>, world: &World, id: U256) -> IGem::GemData {
    let result = dispatch(
        s.clone(),
        world.readers(),
        &IGem::getGemStatusCall { gemId: id }.abi_encode(),
        Address::ZERO,
        U256::ZERO,
    )
    .unwrap();
    IGem::getGemStatusCall::abi_decode_returns(&result).unwrap()
}

fn agrees(s: &StorageHandle<'_>, world: &World, id: U256, expected: u8) {
    let before = api::get_view(s, &world.scope, &world.parent, id)
        .unwrap()
        .unwrap();
    assert_eq!(status(s, world, id).state, expected);
    assert_eq!(api::is_qualified(s, &before.item).unwrap(), expected == 1);
    let after = api::get_view(s, &world.scope, &world.parent, id)
        .unwrap()
        .unwrap();
    assert_eq!(after, before, "view must not store Qualified");
    assert_eq!(before.item.state, GemState::Issued as u8);
}

#[test]
fn qualified_gem_view_uses_the_permanent_full_day_reference_predicate() {
    let (mut provider, world) = provider();
    StorageHandle::enter(&mut provider, |s| {
        let id = issue(&s, &world);
        agrees(&s, &world, id, 0);
        let first = first_full_day(ISSUED);
        close(&s, timestamp_to_date_key(ISSUED), 840, FLOOR + 1);
        agrees(&s, &world, id, 0); // Partial issuance day cannot qualify.
        close(&s, first, 978, FLOOR + 100);
        agrees(&s, &world, id, 0); // Issuance currency cannot qualify the reference currency.
        close(&s, first, 840, FLOOR);
        agrees(&s, &world, id, 0); // Strictly above, not equal.
        let second = next_date_key(first);
        close(&s, second, 840, FLOOR + 1);
        agrees(&s, &world, id, 1);
        close(&s, next_date_key(second), 840, FLOOR - 1);
        agrees(&s, &world, id, 1); // Later price declines cannot undo qualification.
    });
}

#[test]
fn qualified_gem_projection_preserves_called_deadline_and_settled_states() {
    let (mut provider, world) = provider();
    let (id, deadline) = StorageHandle::enter(&mut provider, |s| {
        let id = issue(&s, &world);
        close(&s, first_full_day(ISSUED), 840, FLOOR + 1);
        assert_eq!(status(&s, &world, id).state, 1);
        let bucket = api::get_item(&s, &world.scope, &world.parent, id)
            .unwrap()
            .unwrap()
            .bucket_key;
        let mut contract = GemContract::new(s.clone());
        let terms = contract.read_call_terms(bucket).unwrap();
        contract.mark_bucket_called(bucket, &terms, ISSUED).unwrap();
        (id, ISSUED + u64::from(terms.call_notice_period_seconds))
    });
    provider.set_timestamp(U256::from(deadline));
    StorageHandle::enter(&mut provider, |s| {
        assert_eq!(status(&s, &world, id).state, 2);
    });
    provider.set_timestamp(U256::from(deadline + 1));
    StorageHandle::enter(&mut provider, |s| {
        assert_eq!(status(&s, &world, id).state, 4);
        assert!(
            api::settle_gem(&s, &world.scope, &world.parent, id).is_err(),
            "past the deadline nothing can be paid"
        );
    });
    provider.set_timestamp(U256::from(deadline));
    StorageHandle::enter(&mut provider, |s| {
        api::settle_gem(&s, &world.scope, &world.parent, id).unwrap();
        assert_eq!(status(&s, &world, id).state, 3);
    });
    provider.set_timestamp(U256::from(deadline + 1));
    StorageHandle::enter(&mut provider, |s| {
        let data = status(&s, &world, id);
        assert_eq!(data.state, 3, "paid stays paid past the deadline");
        assert_eq!(data.calledAt, ISSUED);
        assert_eq!(data.settlementDeadline, deadline);
    });
}
