use super::*;

#[test]
fn a_series_called_in_the_same_block_cannot_be_sent() {
    assert_called_source_cannot_be_sent(T_NOW);
}

#[test]
fn a_called_series_at_its_deadline_cannot_be_sent() {
    assert_called_source_cannot_be_sent(T_NOW - u64::from(SOURCE_NOTICE_SECONDS));
}

#[test]
fn a_series_past_its_deadline_cannot_be_sent() {
    assert_called_source_cannot_be_sent(T_NOW - u64::from(SOURCE_NOTICE_SECONDS) - 1);
}

#[test]
fn an_issued_series_is_sent_once_without_a_promis_limit_credit() {
    with_storage(Some(six_decimal_unit()), |storage| {
        seed_source_series(
            storage,
            six_decimal_unit(),
            six_decimal_u128(),
            outbe_intex::IntexCallTrigger::default(),
        );
        let promis_limit = unallocated(storage);

        let id = send_whole_holding(storage).unwrap();

        let capacity = sent_capacity(six_decimal_u128());
        let factory = GemFactoryContract::new(storage.clone());
        assert_eq!(
            factory
                .positions
                .get(id)
                .unwrap()
                .unwrap()
                .remaining_capacity_minor,
            capacity
        );
        assert_eq!(factory.total_capacity_minor.read().unwrap(), capacity);
        assert!(
            !outbe_intex::api::series_exists(storage, source_series_id()).unwrap(),
            "every unit went into the position, so the source series is spent"
        );
        assert_eq!(unallocated(storage), promis_limit);
    });
}

#[test]
fn create_gem_factory_position_burns_the_units_and_records_its_owner() {
    with_storage(None, |storage| {
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        let capacity = sent_capacity(six_decimal_u128());

        let factory = GemFactoryContract::new(storage.clone());
        let rec = factory.positions.get(id).unwrap().unwrap();
        assert_eq!(rec.owner, ALICE);
        assert_eq!(rec.source_series_id, source_series_id());
        assert_eq!(rec.remaining_capacity_minor, capacity);
        assert_eq!(factory.total_capacity_minor.read().unwrap(), capacity);
        assert_eq!(runtime::position_data(storage, id).unwrap().owner, ALICE);
    });
}

#[test]
fn sending_every_unit_deletes_the_spent_source_series() {
    with_storage(None, |storage| {
        seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        // Their load lives in the position now, so nothing is left in the source series.
        assert!(!outbe_intex::api::series_exists(storage, source_series_id()).unwrap());
    });
}

#[test]
fn a_second_position_of_one_owner_and_series_in_a_block_is_refused() {
    with_storage(None, |storage| {
        seed_source_series(
            storage,
            six_decimal_unit(),
            six_decimal_u128(),
            outbe_intex::IntexCallTrigger::default(),
        );
        let half = U256::from(SENT_UNITS / 2);
        runtime::create_gem_factory_position(storage, ALICE, source_series_id(), half).unwrap();
        assert_eq!(
            runtime::create_gem_factory_position(storage, ALICE, source_series_id(), half)
                .unwrap_err()
                .to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::GemFactoryPositionAlreadyExists
            )
            .to_string()
        );
    });
}

/// An expired position reads empty at once, returns its remainder with its day and is
/// then deleted.
#[test]
fn an_expired_position_returns_its_remainder() {
    with_storage(None, |storage| {
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        let capacity = sent_capacity(six_decimal_u128());
        let expires = T_NOW + POSITION_VALIDITY_SECONDS;

        daily_pass(&block_ctx(storage, expires)).unwrap();
        assert_eq!(unallocated(storage), U256::ZERO);
        storage.set_block_timestamp(U256::from(expires)).unwrap();
        assert_eq!(
            runtime::position_data(storage, id)
                .unwrap()
                .remainingCapacityMinor,
            U256::ZERO
        );

        let block = daily_pass(&block_ctx(storage, day_closed(expires))).unwrap();
        assert_eq!(unallocated(storage), capacity);
        assert_eq!(
            position_expiry::delete_expired_positions(&block).unwrap(),
            1
        );

        let factory = GemFactoryContract::new(storage.clone());
        assert!(factory.positions.get(id).unwrap().is_none());
        assert!(runtime::position_data(storage, id).is_err());
    });
}

#[test]
fn position_of_15_units_issues_6_and_gives_back_9() {
    let unit = six_decimal_unit();
    let mut provider = test_storage(Some(unit));
    let id = StorageHandle::enter(&mut provider, |storage| {
        let storage = &storage;
        seed_day_vwap(storage, 840, unit);
        let promis_load = 15 * six_decimal_u128() / u128::from(SENT_UNITS);
        let id = seed_and_send(storage, unit, promis_load);
        assert_eq!(sent_capacity(promis_load), U256::from(15u64) * unit);
        let gem_id = runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id: id,
                owner: BOB,
                promis_load_minor: U256::from(6u64) * unit,
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();

        let closed = day_closed(T_NOW + POSITION_VALIDITY_SECONDS);
        let block = daily_pass(&block_ctx(storage, closed)).unwrap();
        assert_eq!(unallocated(storage), U256::from(9u64) * unit);
        assert_eq!(
            position_expiry::delete_expired_positions(&block).unwrap(),
            1
        );
        assert!(GemFactoryContract::new(storage.clone())
            .positions
            .get(id)
            .unwrap()
            .is_none());
        assert_eq!(gem_view(storage, gem_id).item.owner, BOB);

        daily_pass(&block_ctx(storage, closed + SECONDS_PER_DAY)).unwrap();
        assert_eq!(unallocated(storage), U256::from(9u64) * unit);
        id
    });

    let events = provider.get_ordered_events();
    let credits: Vec<_> = events
        .iter()
        .filter(|log| log.address == outbe_system_common::addresses::PROMIS_LIMIT_ADDRESS)
        .filter_map(|log| {
            outbe_promislimit::precompile::IPromisLimit::PromisLimitAdded::decode_log(log)
                .ok()
                .map(|log| log.data)
        })
        .filter(|event| event.source == outbe_promislimit::LimitSource::GemFactoryPosition as u8)
        .map(|event| event.amountMinor)
        .collect();
    assert_eq!(credits, vec![U256::from(9u64) * unit]);
    let expired: Vec<_> = events
        .iter()
        .filter(|log| log.address == outbe_system_common::addresses::GEM_FACTORY_ADDRESS)
        .filter_map(|log| {
            crate::precompile::IGemFactory::GemFactoryPositionExpired::decode_log(log)
                .ok()
                .map(|log| log.data)
        })
        .map(|event| (event.gemFactoryPositionId, event.owner))
        .collect();
    assert_eq!(expired, vec![(id, ALICE)]);
}

/// A storage fault is this node's own: it fails the block instead of skipping the day.
#[test]
fn a_storage_fault_in_the_position_sweep_fails_the_block() {
    let mut provider = test_storage(None);
    let id = StorageHandle::enter(&mut provider, |storage| {
        seed_and_send(&storage, six_decimal_unit(), six_decimal_u128())
    });

    provider.fail_mutation_at_address(outbe_system_common::addresses::PROMIS_LIMIT_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = block_ctx(&storage, day_closed(T_NOW + POSITION_VALIDITY_SECONDS));
        assert!(matches!(
            daily_pass(&ctx),
            Err(outbe_primitives::error::PrecompileError::Storage(_))
        ));
        let factory = GemFactoryContract::new(storage.clone());
        let record = factory.positions.get(id).unwrap().unwrap();
        assert_ne!(
            factory
                .expiry_day_capacity_minor
                .read(&record.expiry_day().unwrap())
                .unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn a_drained_position_returns_nothing_and_is_deleted() {
    with_storage(Some(six_decimal_unit()), |storage| {
        seed_day_vwap(storage, 840, six_decimal_unit());
        let id = seed_and_send(storage, six_decimal_unit(), six_decimal_u128());
        runtime::issue_merchant_gem(
            storage,
            crate::schema::MerchantGemIssueParams {
                caller: ALICE,
                gem_factory_position_id: id,
                owner: BOB,
                promis_load_minor: sent_capacity(six_decimal_u128()),
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();

        let block = daily_pass(&block_ctx(
            storage,
            day_closed(T_NOW + POSITION_VALIDITY_SECONDS),
        ))
        .unwrap();
        assert_eq!(unallocated(storage), U256::ZERO);
        assert_eq!(
            position_expiry::delete_expired_positions(&block).unwrap(),
            1
        );
        assert!(GemFactoryContract::new(storage.clone())
            .positions
            .get(id)
            .unwrap()
            .is_none());
    });
}

#[test]
fn issue_gem_position_unknown_source_rejects() {
    with_storage(None, |storage| {
        let r = runtime::create_gem_factory_position(
            storage,
            ALICE,
            source_series_id(),
            U256::from(SENT_UNITS),
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::SourceIntexNotFound
            )
            .to_string()
        );
    });
}
