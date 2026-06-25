use super::*;
use outbe_common::WorldwideDay as WwdKey;

#[test]
fn test_create_worldwide_day() {
    with_contract(|m| {
        let wwd = WwdKey::new(20241220);
        let start = 1000u64;
        let lookback_h = LOOKBACK_DELAY_HOURS;
        let offering_h = OFFERING_PERIOD_HOURS;

        m.create_worldwide_day(wwd, start, lookback_h, offering_h)
            .unwrap();

        let forming_end = start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let lookback_end = forming_end + lookback_h * SECONDS_PER_HOUR;
        let offering_end = lookback_end + offering_h * SECONDS_PER_HOUR;
        let scheduled = offering_end + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;

        assert_eq!(
            m.worldwide_days.entry(wwd).forming_start().read().unwrap(),
            start
        );
        assert_eq!(
            m.worldwide_days.entry(wwd).forming_end().read().unwrap(),
            forming_end
        );
        assert_eq!(
            m.worldwide_days.entry(wwd).lookback_end().read().unwrap(),
            lookback_end
        );
        assert_eq!(
            m.worldwide_days.entry(wwd).offering_end().read().unwrap(),
            offering_end
        );
        assert_eq!(
            m.worldwide_days
                .entry(wwd)
                .scheduled_process_time()
                .read()
                .unwrap(),
            scheduled
        );

        assert_eq!(m.get_wwd_status(wwd).unwrap(), status::FORMING);
        assert_eq!(m.get_wwd_day_type(wwd).unwrap(), day_type::UNKNOWN);
    });
}

#[test]
fn test_wwd_status_transitions() {
    with_contract(|m| {
        let wwd = WwdKey::new(20241220);
        let start = 1000u64;
        m.create_worldwide_day(wwd, start, LOOKBACK_DELAY_HOURS, OFFERING_PERIOD_HOURS)
            .unwrap();

        let forming_end = start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let lookback_end = forming_end + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR;
        let offering_end = lookback_end + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let scheduled = offering_end + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;

        assert_eq!(
            m.update_wwd_status(wwd, start + 100).unwrap(),
            status::FORMING
        );
        assert_eq!(
            m.update_wwd_status(wwd, forming_end).unwrap(),
            status::LOOKBACK_DELAY
        );
        assert_eq!(
            m.update_wwd_status(wwd, lookback_end).unwrap(),
            status::OFFERING
        );
        assert_eq!(
            m.update_wwd_status(wwd, offering_end).unwrap(),
            status::WAITING
        );
        assert_eq!(m.update_wwd_status(wwd, scheduled).unwrap(), status::READY);

        m.mark_wwd_completed(wwd).unwrap();
        assert_eq!(
            m.update_wwd_status(wwd, scheduled + 999999).unwrap(),
            status::COMPLETED
        );
    });
}

#[test]
fn test_timestamp_to_date_key() {
    assert_eq!(timestamp_to_date_key(1734652800u64), 20241220);
    assert_eq!(timestamp_to_date_key(1704067200u64), 20240101);
    assert_eq!(timestamp_to_date_key(0), 19700101);
}

#[test]
fn test_wwd_from_timestamp() {
    assert_eq!(WwdKey::from_timestamp(1734649200u64), WwdKey::new(20241220));
    assert_eq!(WwdKey::from_timestamp(1734688800u64), WwdKey::new(20241221));
}

#[test]
fn test_set_metadosis_limit_overwrites() {
    with_contract(|m| {
        let date = WwdKey::new(20241220);

        // The per-WWD limit is now a single field on the WorldwideDay record,
        // not a separate accumulating map: each write overwrites the prior value.
        m.set_metadosis_limit(date, U256::from(100u64)).unwrap();
        assert_eq!(
            m.worldwide_days
                .entry(date)
                .metadosis_limit_amount()
                .read()
                .unwrap(),
            U256::from(100u64)
        );

        m.set_metadosis_limit(date, U256::from(250u64)).unwrap();
        assert_eq!(
            m.worldwide_days
                .entry(date)
                .metadosis_limit_amount()
                .read()
                .unwrap(),
            U256::from(250u64)
        );
    });
}

#[test]
fn test_active_wwd_add_remove() {
    with_contract(|m| {
        m.add_active_wwd(WwdKey::new(20241218)).unwrap();
        m.add_active_wwd(WwdKey::new(20241219)).unwrap();
        m.add_active_wwd(WwdKey::new(20241220)).unwrap();

        let active = m.active_wwd.read_all().unwrap();
        assert_eq!(active.len(), 3);
        assert!(active.contains(&WwdKey::new(20241218)));
        assert!(active.contains(&WwdKey::new(20241219)));
        assert!(active.contains(&WwdKey::new(20241220)));

        m.remove_active_wwd(WwdKey::new(20241219)).unwrap();

        let active = m.active_wwd.read_all().unwrap();
        assert_eq!(active.len(), 2);
        assert!(active.contains(&WwdKey::new(20241218)));
        assert!(active.contains(&WwdKey::new(20241220)));
        assert!(!active.contains(&WwdKey::new(20241219)));

        m.remove_active_wwd(WwdKey::new(20241218)).unwrap();
        m.remove_active_wwd(WwdKey::new(20241220)).unwrap();
        assert!(m.active_wwd.read_all().unwrap().is_empty());
    });
}

#[test]
fn test_calculate_metadosis_green_day() {
    with_contract(|m| {
        let wwd = WwdKey::new(20241220);
        m.worldwide_days
            .entry(wwd)
            .status()
            .write(status::READY)
            .unwrap();
        m.worldwide_days
            .entry(wwd)
            .day_type()
            .write(day_type::GREEN)
            .unwrap();

        let tribute_total = U256::from(10_000u64);
        let day_limit = U256::from(5_000u64);

        let calc = m
            .calculate_metadosis(wwd, tribute_total, day_limit)
            .unwrap();

        // SYMBOLIC_RATE = 32, GREEN day:
        //   demand     = 10_000 * 32 / 100 = 3_200
        //   limit      = day_limit         = 5_000
        //   allocation = min(demand, limit) = 3_200
        //   remainder  = day_limit - allocation = 1_800
        assert_eq!(calc.day_gratis_allocation, U256::from(3_200u64));
        assert_eq!(calc.day_metadosis_limit_remainder, U256::from(1_800u64));
    });
}

#[test]
fn test_calculate_metadosis_red_day() {
    with_contract(|m| {
        let wwd = WwdKey::new(20241220);
        m.worldwide_days
            .entry(wwd)
            .status()
            .write(status::READY)
            .unwrap();
        m.worldwide_days
            .entry(wwd)
            .day_type()
            .write(day_type::RED)
            .unwrap();

        let tribute_total = U256::from(10_000u64);
        let day_limit = U256::from(5_000u64);

        let calc = m
            .calculate_metadosis(wwd, tribute_total, day_limit)
            .unwrap();
        let (allocation, remainder) = (
            calc.day_gratis_allocation,
            calc.day_metadosis_limit_remainder,
        );

        // SYMBOLIC_RATE = 32, RED_DAY_REDUCTION_COEF = 8, RED day:
        //   demand     = 10_000 * 32 / 100 / 8 = 400
        //   limit      = day_limit / 8         = 625
        //   allocation = min(demand, limit)     = 400
        //   remainder  = day_limit - allocation = 4_600
        assert_eq!(allocation, U256::from(400u64));
        assert_eq!(remainder, U256::from(4_600u64));
    });
}

#[test]
fn test_calculate_metadosis_unknown_day_type_errors() {
    with_contract(|m| {
        let wwd = WwdKey::new(20241220);
        m.worldwide_days
            .entry(wwd)
            .status()
            .write(status::READY)
            .unwrap();
        assert_eq!(
            m.worldwide_days.entry(wwd).day_type().read().unwrap(),
            day_type::UNKNOWN
        );

        let tribute_total = U256::from(10_000u64);
        let day_limit = U256::from(5_000u64);

        assert!(m
            .calculate_metadosis(wwd, tribute_total, day_limit)
            .is_err());
    });
}

#[test]
fn test_bootstrap_effective_hours_depend_on_chain_identity() {
    with_contract(|m| {
        let bootstrap_end = 100_000u64;
        m.set_bootstrap_end_time(bootstrap_end).unwrap();

        let (lookback, offering) = m
            .effective_hours(outbe_primitives::chain::CHAIN_ID)
            .unwrap();
        assert_eq!(lookback, BOOTSTRAP_LOOKBACK_DELAY_HOURS);
        assert_eq!(offering, BOOTSTRAP_OFFERING_PERIOD_HOURS);

        let (lookback, offering) = m
            .effective_hours(outbe_primitives::chain::TESTNET_CHAIN_ID)
            .unwrap();
        assert_eq!(lookback, BOOTSTRAP_LOOKBACK_DELAY_HOURS);
        assert_eq!(offering, BOOTSTRAP_OFFERING_PERIOD_HOURS);

        let (lookback, offering) = m.effective_hours(CHAIN_ID).unwrap();
        assert_eq!(lookback, LOOKBACK_DELAY_HOURS);
        assert_eq!(offering, OFFERING_PERIOD_HOURS);
    });
}

#[test]
fn test_query_worldwide_days_by_status_via_precompile() {
    with_storage(|storage| {
        let mut metadosis = MetadosisContract::new(storage.clone());
        for (wwd, st) in [
            (WwdKey::new(20260320), status::OFFERING),
            (WwdKey::new(20260321), status::OFFERING),
            (WwdKey::new(20260322), status::FORMING),
        ] {
            metadosis
                .worldwide_days
                .entry(wwd)
                .status()
                .write(st)
                .unwrap();
            metadosis.add_active_wwd(wwd).unwrap();
        }

        let call_data = IMetadosis::getWorldwideDaysByStatusCall {
            status: status::OFFERING,
        }
        .abi_encode();
        let encoded = metadosis_dispatch(storage, &call_data, Address::ZERO, U256::ZERO).unwrap();
        let decoded =
            IMetadosis::getWorldwideDaysByStatusCall::abi_decode_returns(&encoded).unwrap();

        assert_eq!(decoded.len(), 2);
        assert!(decoded.contains(&20260320));
        assert!(decoded.contains(&20260321));
    });
}

// status transition guards
// ---------------------------------------------------------------------------

fn setup_wwd_at_status(m: &mut MetadosisContract, wwd: WwdKey, target: u8) {
    m.create_worldwide_day(wwd, 1000, LOOKBACK_DELAY_HOURS, OFFERING_PERIOD_HOURS)
        .unwrap();
    m.worldwide_days.entry(wwd).status().write(target).unwrap();
}

#[test]
fn test_mark_completed_rejects_non_ready_status() {
    with_contract(|m| {
        let wwd = WwdKey::new(20260101);
        setup_wwd_at_status(m, wwd, status::FORMING);
        let err = m.mark_wwd_completed(wwd).unwrap_err();
        assert!(err.to_string().contains("COMPLETED"));
        assert_eq!(m.get_wwd_status(wwd).unwrap(), status::FORMING);
    });
}

#[test]
fn test_mark_completed_rejects_already_failed_day() {
    with_contract(|m| {
        let wwd = WwdKey::new(20260102);
        setup_wwd_at_status(m, wwd, status::FAILED);
        assert!(m.mark_wwd_completed(wwd).is_err());
        assert_eq!(m.get_wwd_status(wwd).unwrap(), status::FAILED);
    });
}

#[test]
fn test_mark_completed_allows_ready() {
    with_contract(|m| {
        let wwd = WwdKey::new(20260103);
        setup_wwd_at_status(m, wwd, status::READY);
        m.mark_wwd_completed(wwd).unwrap();
        assert_eq!(m.get_wwd_status(wwd).unwrap(), status::COMPLETED);
    });
}

#[test]
fn test_mark_failed_rejects_completed_day() {
    with_contract(|m| {
        let wwd = WwdKey::new(20260104);
        setup_wwd_at_status(m, wwd, status::COMPLETED);
        let err = m.mark_wwd_failed(wwd).unwrap_err();
        assert!(err.to_string().contains("COMPLETED"));
        assert_eq!(m.get_wwd_status(wwd).unwrap(), status::COMPLETED);
    });
}

#[test]
fn test_mark_failed_allows_any_non_completed_status() {
    with_contract(|m| {
        for (i, source) in [
            status::FORMING,
            status::LOOKBACK_DELAY,
            status::OFFERING,
            status::WAITING,
            status::READY,
            status::IN_PROGRESS,
            status::FAILED, // idempotent re-fail
        ]
        .iter()
        .enumerate()
        {
            let wwd = WwdKey::new(20260200u32 + i as u32);
            setup_wwd_at_status(m, wwd, *source);
            m.mark_wwd_failed(wwd).unwrap();
            assert_eq!(m.get_wwd_status(wwd).unwrap(), status::FAILED);
        }
    });
}

#[test]
fn test_storage_dsl_layout_slots() {
    with_contract(|m| {
        assert_eq!(m.bootstrap_end_time.slot(), U256::ZERO);
        assert_eq!(m.worldwide_days.base_slot(), U256::from(1u64));
        // WorldwideDay gained `metadosis_limit_amount`, so the record is now
        // 10 scalar slots (was 9); worldwide_days occupies slots 1..=10.
        assert_eq!(<WorldwideDay as StorageRecord>::SLOTS, 10);
        assert_eq!(m.active_wwd_count.slot(), U256::from(11u64));
        // `active_wwd` is a Set (2 slots: 12 = length, 13 = positions), so the
        // next schema field lands at 14 — this pins the Set's position too.
        assert_eq!(m.config_oracle_pair_hash.slot(), U256::from(14u64));
        // `closed_worldwidedays` is a Deque (2 slots: 15 = begin, 16 = end).
        assert_eq!(m.closed_worldwidedays.base_slot(), U256::from(15u64));
    });
}

#[test]
fn test_mark_terminal_retires_and_is_idempotent() {
    with_contract(|m| {
        let wwd = WwdKey::new(20240101);
        m.create_worldwide_day(wwd, 1000, LOOKBACK_DELAY_HOURS, OFFERING_PERIOD_HOURS)
            .unwrap();
        m.worldwide_days
            .entry(wwd)
            .status()
            .write(status::READY)
            .unwrap();
        m.mark_wwd_completed(wwd).unwrap();

        // Terminal day leaves the active set, enters the delete-queue, and the
        // record stays readable (under the cap, not yet evicted).
        assert!(!m.active_wwd.read_all().unwrap().contains(&wwd));
        assert_eq!(m.closed_worldwidedays.len().unwrap(), 1);
        assert_eq!(m.get_wwd_status(wwd).unwrap(), status::COMPLETED);
        assert!(m
            .get_active_wwd_by_status(status::COMPLETED)
            .unwrap()
            .contains(&wwd));

        // Idempotent re-fail of an already-terminal day must not re-enqueue.
        let wwd2 = WwdKey::new(20240102);
        m.create_worldwide_day(wwd2, 1000, LOOKBACK_DELAY_HOURS, OFFERING_PERIOD_HOURS)
            .unwrap();
        m.worldwide_days
            .entry(wwd2)
            .status()
            .write(status::OFFERING)
            .unwrap();
        m.mark_wwd_failed(wwd2).unwrap();
        assert_eq!(m.closed_worldwidedays.len().unwrap(), 2);
        m.mark_wwd_failed(wwd2).unwrap(); // already FAILED
        assert_eq!(m.closed_worldwidedays.len().unwrap(), 2);
    });
}

#[test]
fn test_terminal_records_capped_oldest_evicted() {
    use alloy_sol_types::SolEvent;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let n = MAX_RECORDS_KEPT as u32 + 2;
    StorageHandle::enter(&mut storage, |handle| {
        let mut m = MetadosisContract::new(handle.clone());
        for i in 0..n {
            let wwd = WwdKey::new(20240101 + i);
            m.create_worldwide_day(wwd, 1000, LOOKBACK_DELAY_HOURS, OFFERING_PERIOD_HOURS)
                .unwrap();
            m.worldwide_days
                .entry(wwd)
                .status()
                .write(status::READY)
                .unwrap();
            m.mark_wwd_completed(wwd).unwrap();
        }

        // Every day retired out of the active set.
        assert!(m.active_wwd.read_all().unwrap().is_empty());
        // Delete-queue capped at MAX_RECORDS_KEPT.
        assert_eq!(
            m.closed_worldwidedays.len().unwrap(),
            MAX_RECORDS_KEPT as u64
        );
        // The two oldest records were evicted and deleted.
        for i in 0..2u32 {
            let wwd = WwdKey::new(20240101 + i);
            assert_eq!(
                m.worldwide_days.entry(wwd).forming_start().read().unwrap(),
                0
            );
        }
        // The newest record is retained.
        let newest = WwdKey::new(20240101 + n - 1);
        assert_ne!(
            m.worldwide_days
                .entry(newest)
                .forming_start()
                .read()
                .unwrap(),
            0
        );
        // Status query over terminal days resolves the kept cap.
        assert_eq!(
            m.get_active_wwd_by_status(status::COMPLETED).unwrap().len(),
            MAX_RECORDS_KEPT
        );
    });

    // Exactly two records evicted ⇒ two WorldwideDayCleanedUp events.
    let cleaned = storage
        .get_events(outbe_primitives::addresses::METADOSIS_ADDRESS)
        .iter()
        .filter(|log| {
            log.topics().first() == Some(&IMetadosis::WorldwideDayCleanedUp::SIGNATURE_HASH)
        })
        .count();
    assert_eq!(cleaned, 2);
}
