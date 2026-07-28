use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolError;
use outbe_primitives::{
    addresses::STABLECOIN_FACTORY_ADDRESS,
    error::PrecompileError,
    stablecoin_fork::{STABLECOIN_LIST_PAGE_CAP, STABLECOIN_V1_SCHEMA_VERSION},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

use crate::{
    abi::IStablecoinFactory as I, api::FactoryReservation, schema::StablecoinFactoryContract,
};

fn with_factory(test: impl FnOnce(StorageHandle<'_>, StablecoinFactoryContract<'_>)) {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        test(storage.clone(), StablecoinFactoryContract::new(storage));
    });
}

fn reservation(
    factory: &StablecoinFactoryContract<'_>,
    proposal_id: u64,
    issuer: Address,
    ticker: &str,
) -> FactoryReservation {
    let (token_id, token) = factory.predict_token_address(issuer, ticker).unwrap();
    FactoryReservation {
        proposal_id: U256::from(proposal_id),
        token_id,
        ticker: ticker.to_owned(),
        token,
    }
}

fn assert_revert<E: SolError>(error: PrecompileError) {
    match error {
        PrecompileError::RevertBytes(bytes) => assert_eq!(&bytes[..4], E::SELECTOR),
        other => panic!("expected revert, got {other:?}"),
    }
}

#[test]
fn v1_raw_layout_and_pristine_schema_are_pinned() {
    with_factory(|storage, factory| {
        assert_eq!(factory.schema_version.slot(), U256::ZERO);
        assert_eq!(factory.token_by_id_index.base_slot(), U256::from(3u64));
        assert_eq!(factory.token_by_ticker_index.base_slot(), U256::from(4u64));
        assert_eq!(factory.token_id_of_index.base_slot(), U256::from(5u64));
        assert_eq!(factory.pending_token_id.base_slot(), U256::from(6u64));
        assert_eq!(factory.pending_ticker.base_slot(), U256::from(7u64));
        assert_eq!(factory.pending_address.base_slot(), U256::from(8u64));
        assert_eq!(factory.reservations.base_slot(), U256::from(9u64));
        assert_eq!(factory.token_count().unwrap(), U256::ZERO);

        let item = reservation(&factory, 1, Address::repeat_byte(0x11), "USD1");
        let mut factory = factory;
        factory.reserve(&item).unwrap();
        assert_eq!(
            storage
                .sload(STABLECOIN_FACTORY_ADDRESS, U256::ZERO)
                .unwrap(),
            U256::from(STABLECOIN_V1_SCHEMA_VERSION)
        );
    });
}

#[test]
fn reservation_release_and_consume_keep_all_indexes_inverse() {
    with_factory(|_storage, mut factory| {
        let released = reservation(&factory, 1, Address::repeat_byte(0x11), "USD1");
        factory.reserve(&released).unwrap();
        assert_eq!(
            factory.pending_token_id.read(&released.token_id).unwrap(),
            released.proposal_id
        );
        assert_eq!(
            factory
                .pending_ticker
                .read(&keccak256(released.ticker.as_bytes()))
                .unwrap(),
            released.proposal_id
        );
        assert_eq!(
            factory.pending_address.read(&released.token).unwrap(),
            released.proposal_id
        );

        let record = factory.release(released.proposal_id).unwrap();
        assert_eq!(record.token_id, released.token_id);
        assert!(factory
            .pending_token_id
            .read(&released.token_id)
            .unwrap()
            .is_zero());
        assert!(factory
            .pending_ticker
            .read(&record.ticker_hash)
            .unwrap()
            .is_zero());
        assert!(factory
            .pending_address
            .read(&released.token)
            .unwrap()
            .is_zero());
        assert!(!factory.reservations.exists(released.proposal_id).unwrap());

        let registered = reservation(&factory, 2, Address::repeat_byte(0x22), "EUR1");
        factory.reserve(&registered).unwrap();
        factory
            .consume_and_register(registered.proposal_id)
            .unwrap();

        assert_eq!(factory.token_count().unwrap(), U256::from(1u64));
        assert_eq!(
            factory.token_by_id(registered.token_id).unwrap(),
            registered.token
        );
        assert_eq!(
            factory.token_by_ticker(&registered.ticker).unwrap(),
            registered.token
        );
        assert_eq!(
            factory.token_id_of(registered.token).unwrap(),
            registered.token_id
        );
        assert!(factory.is_stablecoin(registered.token).unwrap());
        assert!(factory
            .pending_token_id
            .read(&registered.token_id)
            .unwrap()
            .is_zero());
        assert!(!factory.reservations.exists(registered.proposal_id).unwrap());
    });
}

#[test]
fn ticker_is_global_across_issuers_and_pending_address_reserves_full_id() {
    with_factory(|_storage, mut factory| {
        let first = reservation(&factory, 1, Address::repeat_byte(0x11), "USD1");
        factory.reserve(&first).unwrap();

        let second = reservation(&factory, 2, Address::repeat_byte(0x22), "USD1");
        assert_ne!(first.token_id, second.token_id);
        assert_revert::<I::TickerReserved>(
            factory
                .reserve(&second)
                .expect_err("ticker is globally pending"),
        );

        let address_collision = FactoryReservation {
            proposal_id: U256::from(3u64),
            token_id: B256::repeat_byte(0x33),
            ticker: "EUR1".into(),
            token: first.token,
        };
        assert_revert::<I::TokenAddressCollision>(
            factory
                .reserve(&address_collision)
                .expect_err("predicted address is already reserved"),
        );

        factory.consume_and_register(first.proposal_id).unwrap();
        let third = reservation(&factory, 4, Address::repeat_byte(0x44), "USD1");
        assert_revert::<I::TickerAlreadyRegistered>(
            factory
                .reserve(&third)
                .expect_err("ticker remains globally permanent"),
        );
    });
}

#[test]
fn list_accepts_any_limit_from_one_through_one_hundred_and_clamps_pages() {
    with_factory(|_storage, mut factory| {
        assert_revert::<I::InvalidListLimit>(
            factory
                .list_tokens(U256::ZERO, U256::ZERO)
                .expect_err("zero limit"),
        );
        assert_revert::<I::InvalidListLimit>(
            factory
                .list_tokens(U256::ZERO, U256::from(STABLECOIN_LIST_PAGE_CAP + 1))
                .expect_err("limit above cap"),
        );

        for index in 0..101u64 {
            let item = reservation(
                &factory,
                index + 1,
                Address::from_word(U256::from(index + 1).into()),
                &format!("T{index:03}"),
            );
            factory.reserve(&item).unwrap();
            factory.consume_and_register(item.proposal_id).unwrap();
        }

        assert_eq!(
            factory
                .list_tokens(U256::ZERO, U256::from(1u64))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            factory
                .list_tokens(U256::ZERO, U256::from(100u64))
                .unwrap()
                .len(),
            100
        );
        assert_eq!(
            factory
                .list_tokens(U256::from(100u64), U256::from(10u64))
                .unwrap()
                .len(),
            1
        );
        assert!(factory
            .list_tokens(U256::from(101u64), U256::from(10u64))
            .unwrap()
            .is_empty());
        assert!(factory
            .list_tokens(U256::MAX, U256::from(10u64))
            .unwrap()
            .is_empty());
    });
}

#[test]
fn outer_checkpoint_rolls_back_the_complete_reservation_triple() {
    with_factory(|storage, factory| {
        let item = reservation(&factory, 1, Address::repeat_byte(0x11), "USD1");
        let result: outbe_primitives::error::Result<()> = storage.with_checkpoint(|| {
            let mut factory = StablecoinFactoryContract::new(storage.clone());
            factory.reserve(&item)?;
            Err(PrecompileError::Fatal("forced after reservation".into()))
        });
        assert!(matches!(result, Err(PrecompileError::Fatal(_))));

        let factory = StablecoinFactoryContract::new(storage);
        assert!(factory
            .pending_token_id
            .read(&item.token_id)
            .unwrap()
            .is_zero());
        assert!(factory
            .pending_ticker
            .read(&keccak256(item.ticker.as_bytes()))
            .unwrap()
            .is_zero());
        assert!(factory.pending_address.read(&item.token).unwrap().is_zero());
        assert!(!factory.reservations.exists(item.proposal_id).unwrap());
        assert_eq!(factory.schema_version.read().unwrap(), 0);
    });
}

#[test]
fn corrupted_reservation_triple_is_fatal_and_not_partially_cleaned() {
    with_factory(|_storage, mut factory| {
        let item = reservation(&factory, 1, Address::repeat_byte(0x11), "USD1");
        factory.reserve(&item).unwrap();
        factory.pending_address.clear(&item.token).unwrap();

        assert!(matches!(
            factory.release(item.proposal_id),
            Err(PrecompileError::Fatal(_))
        ));
        assert_eq!(
            factory.pending_token_id.read(&item.token_id).unwrap(),
            item.proposal_id
        );
        assert!(factory.reservations.exists(item.proposal_id).unwrap());
    });
}

#[test]
fn mixed_reserve_release_consume_history_preserves_every_inverse() {
    with_factory(|_storage, mut factory| {
        let mut pending = Vec::new();
        let mut expected_permanent = Vec::new();
        let mut rng = 0x5eed_cafe_f00d_beefu64;

        for batch in 0..4u64 {
            for index in 0..32u64 {
                let ordinal = batch * 32 + index;
                let item = reservation(
                    &factory,
                    ordinal + 1,
                    Address::from_word(U256::from(ordinal + 1).into()),
                    &format!("R{ordinal:03}"),
                );
                factory.reserve(&item).unwrap();
                pending.push(item);
            }

            while let Some(item) = pending.pop() {
                rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                if rng & 1 == 0 {
                    factory.release(item.proposal_id).unwrap();
                } else {
                    factory.consume_and_register(item.proposal_id).unwrap();
                    expected_permanent.push(item.clone());
                }

                assert!(factory
                    .pending_token_id
                    .read(&item.token_id)
                    .unwrap()
                    .is_zero());
                assert!(factory
                    .pending_ticker
                    .read(&keccak256(item.ticker.as_bytes()))
                    .unwrap()
                    .is_zero());
                assert!(factory.pending_address.read(&item.token).unwrap().is_zero());
                assert!(!factory.reservations.exists(item.proposal_id).unwrap());
            }
        }

        assert_eq!(
            factory.token_count().unwrap(),
            U256::from(expected_permanent.len())
        );
        for item in expected_permanent {
            assert_eq!(factory.token_by_id(item.token_id).unwrap(), item.token);
            assert_eq!(factory.token_by_ticker(&item.ticker).unwrap(), item.token);
            assert_eq!(factory.token_id_of(item.token).unwrap(), item.token_id);
        }
    });
}

#[test]
fn unknown_schema_and_unknown_reverse_lookup_fail_closed() {
    with_factory(|_storage, factory| {
        factory.schema_version.write(2).unwrap();
        assert!(matches!(
            factory.token_count(),
            Err(PrecompileError::Fatal(_))
        ));
    });

    with_factory(|_storage, factory| {
        assert_revert::<I::UnknownStablecoin>(
            factory
                .token_id_of(Address::repeat_byte(0x99))
                .expect_err("unknown reverse lookup"),
        );
        assert!(!factory.is_stablecoin(Address::repeat_byte(0x99)).unwrap());
    });
}
