use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};
use outbe_nod::{
    api,
    openings::{entry_price_slots, evaluate_entry_prices},
    NodContract,
};
use outbe_primitives::{
    addresses::NOD_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::WorldwideDay,
};

#[test]
fn snapshot_round_trips_and_proves_exact_currency_prices() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let day = WorldwideDay::new(20260715);
        let prices = BTreeMap::from([(840, U256::from(320_000)), (978, U256::from(250_000))]);
        assert_eq!(
            api::entry_price_snapshot(storage.clone(), day).unwrap(),
            None
        );
        api::store_entry_price_snapshot(storage.clone(), day, &prices).unwrap();
        assert_eq!(
            api::entry_price_snapshot(storage.clone(), day).unwrap(),
            Some(prices.clone())
        );
        assert!(api::store_entry_price_snapshot(storage.clone(), day, &BTreeMap::new()).is_err());
        let isos = [826, 840, 978];
        let slots = entry_price_slots(day, &isos).unwrap();
        let raw: Vec<_> = slots
            .iter()
            .map(|slot| {
                (
                    *slot,
                    storage
                        .sload(NOD_ADDRESS, U256::from_be_bytes(slot.0))
                        .unwrap(),
                )
            })
            .collect();
        assert_eq!(evaluate_entry_prices(day, &isos, &raw).unwrap(), prices);
        let mut reordered = raw.clone();
        reordered.swap(1, 2);
        assert!(evaluate_entry_prices(day, &isos, &reordered).is_err());
        let mut unfrozen = raw.clone();
        unfrozen[0].1 = U256::ZERO;
        assert!(evaluate_entry_prices(day, &isos, &unfrozen).is_err());
        let mut wrong_slot = raw;
        wrong_slot[1].0 = B256::ZERO;
        assert!(evaluate_entry_prices(day, &isos, &wrong_slot).is_err());
        assert!(entry_price_slots(day, &[978]).is_err());
        assert!(entry_price_slots(day, &[840, 840]).is_err());
        assert!(entry_price_slots(day, &[0, 840]).is_err());
    });
}

#[test]
fn snapshot_write_is_atomic_at_every_mutation_and_empty_is_frozen() {
    let day = WorldwideDay::new(20260715);
    let prices = BTreeMap::from([(840, U256::from(320_000)), (978, U256::from(250_000))]);
    // Two currency/price writes per row, followed by count and frozen flag.
    for operation in 0..6 {
        let mut provider = HashMapStorageProvider::new(1);
        let before = provider.storage.clone();
        provider.fail_after_mutation_at(operation);
        let result = StorageHandle::enter(&mut provider, |storage| {
            api::store_entry_price_snapshot(storage, day, &prices)
        });
        assert!(result.is_err());
        assert_eq!(provider.storage, before);
    }
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        api::store_entry_price_snapshot(storage.clone(), day, &BTreeMap::new()).unwrap();
        assert_eq!(
            api::entry_price_snapshot(storage.clone(), day).unwrap(),
            Some(BTreeMap::new())
        );
        assert!(api::store_entry_price_snapshot(storage.clone(), day, &prices).is_err());
        NodContract::new(storage.clone())
            .entry_price_currency_count
            .write(&day, 257)
            .unwrap();
        assert!(api::entry_price_snapshot(storage, day).is_err());
    });
}
