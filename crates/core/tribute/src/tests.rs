use alloy_primitives::{address, U256};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::{TributeContract, TributeData};

fn with_tribute<R>(f: impl FnOnce(&mut TributeContract) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        let mut tc = TributeContract::new(storage.clone());
        f(&mut tc)
    })
}

fn with_provider<R>(f: impl FnOnce(&mut HashMapStorageProvider) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    f(&mut storage)
}

fn sample_tribute() -> TributeData {
    TributeData {
        token_id: U256::from(1u64),
        owner: address!("0x1111111111111111111111111111111111111111"),
        worldwide_day: 20241220u32.into(),
        issuance_amount_minor: U256::from(1_000_000_000_000_000_000u128),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(500_000_000_000_000_000u128),
        reference_currency: 840,
        exclude_from_intex_issuance: false,
        tribute_price_minor: U256::from(2_000_000_000_000_000_000u128),
    }
}

fn open_sample_day(tc: &mut TributeContract) {
    tc.unseal_day(20241220u32.into()).unwrap();
}

#[test]
fn test_initial_state() {
    with_tribute(|tc| {
        assert_eq!(tc.total_supply().unwrap(), 0);
        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_amount, U256::ZERO);
        assert!(!totals.is_sealed);
    });
}

#[test]
fn test_issue_requires_initialized_unsealed_day() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        assert!(tc.issue(&tribute).is_err());

        open_sample_day(tc);
        tc.issue(&tribute).unwrap();
        assert_eq!(tc.total_supply().unwrap(), 1);
    });
}

#[test]
fn test_issue_duplicate_fails() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        open_sample_day(tc);
        tc.issue(&tribute).unwrap();
        assert!(tc.issue(&tribute).is_err());
    });
}

#[test]
fn test_day_bucket_tracks_nominal_and_gratis() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);
        t2.nominal_amount_minor = U256::from(300_000_000_000_000_000u128);

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();

        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 2);
        assert_eq!(
            totals.tribute_nominal_amount,
            t1.nominal_amount_minor + t2.nominal_amount_minor
        );
        assert!(!totals.is_sealed);
    });
}

#[test]
fn test_burn_tribute() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        open_sample_day(tc);
        tc.issue(&tribute).unwrap();

        tc.burn(tribute.token_id).unwrap();

        assert_eq!(tc.total_supply().unwrap(), 0);
        assert!(tc.get_tribute(tribute.token_id).unwrap().is_none());

        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_amount, U256::ZERO);
    });
}

#[test]
fn test_burn_nonexistent_fails() {
    with_tribute(|tc| {
        let token_id = U256::from(0x99u64);
        assert!(tc.burn(token_id).is_err());
    });
}

#[test]
fn test_seal_day() {
    with_tribute(|tc| {
        assert!(!tc.is_day_sealed(20241220u32.into()).unwrap());

        tc.seal_day(20241220u32.into()).unwrap();
        assert!(tc.is_day_sealed(20241220u32.into()).unwrap());

        let tribute = sample_tribute();
        assert!(tc.issue(&tribute).is_err());

        tc.unseal_day(20241220u32.into()).unwrap();
        assert!(!tc.is_day_sealed(20241220u32.into()).unwrap());
        tc.issue(&tribute).unwrap();
    });
}

#[test]
fn test_balance_of_tracks_live_owner_tributes() {
    with_tribute(|tc| {
        let alice = address!("0x1111111111111111111111111111111111111111");

        let mut t1 = sample_tribute();
        t1.owner = alice;

        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);
        t2.owner = alice;

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        assert_eq!(tc.balance_of(alice).unwrap(), 2);

        tc.burn(t1.token_id).unwrap();
        assert_eq!(tc.balance_of(alice).unwrap(), 1);
    });
}

#[test]
fn test_token_uri_returns_metadata_json() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        open_sample_day(tc);
        tc.issue(&tribute).unwrap();

        let token_uri = tc.token_uri(tribute.token_id).unwrap();
        assert!(token_uri.starts_with("data:application/json;utf8,"));
        assert!(token_uri.contains("Outbe Tribute"));
        assert!(token_uri.contains("worldwide_day"));
        assert!(token_uri.contains("issuance_amount_minor"));
        assert!(token_uri.contains("\"trait_type\":\"reference_currency\""));
        assert!(token_uri.contains("\"trait_type\":\"exclude_from_intex_issuance\""));
    });
}

#[test]
fn test_get_tribute_ids_by_owner() {
    with_tribute(|tc| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        let mut t1 = sample_tribute();
        t1.owner = alice;

        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);
        t2.owner = alice;

        let mut t3 = sample_tribute();
        t3.token_id = U256::from(3u64);
        t3.owner = bob;

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.issue(&t3).unwrap();

        let alice_ids = tc.get_tribute_ids_by_owner(alice).unwrap();
        assert_eq!(alice_ids, vec![t1.token_id, t2.token_id]);

        let bob_ids = tc.get_tribute_ids_by_owner(bob).unwrap();
        assert_eq!(bob_ids, vec![t3.token_id]);
    });
}

#[test]
fn test_get_tribute_ids_by_day() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();

        let day_ids = tc.get_tribute_ids_by_day(20241220u32.into()).unwrap();
        assert_eq!(day_ids, vec![t1.token_id, t2.token_id]);
    });
}

#[test]
fn test_get_tributes_by_owner_sparse_after_burn() {
    with_tribute(|tc| {
        let alice = address!("0x1111111111111111111111111111111111111111");

        let mut t1 = sample_tribute();
        t1.owner = alice;

        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);
        t2.owner = alice;

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.burn(t1.token_id).unwrap();

        let tributes = tc.get_tributes_by_owner(alice).unwrap();
        assert_eq!(tributes.len(), 1);
        assert_eq!(tributes[0].token_id, t2.token_id);
    });
}

#[test]
fn test_get_all_day_tributes_sparse_after_burn() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.burn(t1.token_id).unwrap();

        let tributes = tc.get_all_day_tributes(20241220u32.into()).unwrap();
        assert_eq!(tributes.len(), 1);
        assert_eq!(tributes[0].token_id, t2.token_id);
    });
}

#[test]
fn test_burn_all_by_wwd() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();

        tc.burn_all_by_wwd(20241220u32.into()).unwrap();

        assert_eq!(tc.total_supply().unwrap(), 0);
        assert!(tc.get_tribute(t1.token_id).unwrap().is_none());
        assert!(tc.get_tribute(t2.token_id).unwrap().is_none());
        assert!(tc
            .get_tribute_ids_by_day(20241220u32.into())
            .unwrap()
            .is_empty());

        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_amount, U256::ZERO);
    });
}

#[test]
fn test_events_emitted_for_issue_and_burn() {
    with_provider(|provider| {
        StorageHandle::enter(provider, |storage| {
            let mut tc = TributeContract::new(storage.clone());
            let tribute = sample_tribute();
            tc.unseal_day(tribute.worldwide_day).unwrap();
            tc.issue(&tribute).unwrap();
            tc.burn(tribute.token_id).unwrap();
        });

        let events = provider.get_events(TRIBUTE_ADDRESS);
        assert_eq!(events.len(), 3, "unseal + issue + burn events expected");
    });
}
