use super::*;
use outbe_common::WorldwideDay as WwdKey;

#[test]
fn test_create_worldwide_day() {
    with_contract(|m| {
        let wwd = WwdKey::new(20241220);
        let start = 1000u64;
        let lookback_h = DEFAULT_LOOKBACK_DELAY_HOURS;
        let offering_h = DEFAULT_OFFERING_PERIOD_HOURS;

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

        assert_eq!(m.get_status(wwd).unwrap(), status::FORMING);
        assert_eq!(m.get_day_type(wwd).unwrap(), day_type::UNKNOWN);
    });
}

#[test]
fn test_wwd_status_transitions() {
    with_contract(|m| {
        let wwd = WwdKey::new(20241220);
        let start = 1000u64;
        m.create_worldwide_day(
            wwd,
            start,
            DEFAULT_LOOKBACK_DELAY_HOURS,
            DEFAULT_OFFERING_PERIOD_HOURS,
        )
        .unwrap();

        let forming_end = start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let lookback_end = forming_end + DEFAULT_LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR;
        let offering_end = lookback_end + DEFAULT_OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR;
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

        m.mark_completed(wwd).unwrap();
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
fn test_record_day_limit_accumulates() {
    with_contract(|m| {
        let date = WwdKey::new(20241220);

        m.record_day_limit(date, U256::from(100u64)).unwrap();
        assert_eq!(m.get_day_limit(date).unwrap(), U256::from(100u64));

        m.record_day_limit(date, U256::from(250u64)).unwrap();
        assert_eq!(m.get_day_limit(date).unwrap(), U256::from(350u64));

        m.record_day_limit(date, U256::from(50u64)).unwrap();
        assert_eq!(m.get_day_limit(date).unwrap(), U256::from(400u64));
    });
}

#[test]
fn test_day_limit_used_flag() {
    with_contract(|m| {
        let date = WwdKey::new(20241220);

        assert!(!m.is_day_limit_used(date).unwrap());
        m.mark_day_limit_used(date).unwrap();
        assert!(m.is_day_limit_used(date).unwrap());
    });
}

#[test]
fn test_active_wwd_add_remove() {
    with_contract(|m| {
        m.add_active_wwd(WwdKey::new(20241218)).unwrap();
        m.add_active_wwd(WwdKey::new(20241219)).unwrap();
        m.add_active_wwd(WwdKey::new(20241220)).unwrap();

        let active = m.get_all_active_wwds().unwrap();
        assert_eq!(active.len(), 3);
        assert!(active.contains(&WwdKey::new(20241218)));
        assert!(active.contains(&WwdKey::new(20241219)));
        assert!(active.contains(&WwdKey::new(20241220)));

        m.remove_active_wwd(WwdKey::new(20241219)).unwrap();

        let active = m.get_all_active_wwds().unwrap();
        assert_eq!(active.len(), 2);
        assert!(active.contains(&WwdKey::new(20241218)));
        assert!(active.contains(&WwdKey::new(20241220)));
        assert!(!active.contains(&WwdKey::new(20241219)));

        m.remove_active_wwd(WwdKey::new(20241218)).unwrap();
        m.remove_active_wwd(WwdKey::new(20241220)).unwrap();
        assert!(m.get_all_active_wwds().unwrap().is_empty());
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
        assert_eq!(lookback, DEFAULT_LOOKBACK_DELAY_HOURS);
        assert_eq!(offering, DEFAULT_OFFERING_PERIOD_HOURS);
    });
}

#[test]
fn test_day_limit_cleanup_keeps_only_latest_30_dates() {
    with_contract(|m| {
        for i in 0..31u32 {
            let date = WwdKey::new(20240101 + i);
            m.record_day_limit(date, U256::from(i + 1)).unwrap();
        }

        let dates = m.get_all_day_limit_dates().unwrap();
        assert_eq!(dates.len(), 30);
        assert!(!dates.contains(&WwdKey::new(20240101)));
        assert!(!m.has_day_limit(WwdKey::new(20240101)).unwrap());
        assert!(dates.contains(&WwdKey::new(20240131)));
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
    m.create_worldwide_day(
        wwd,
        1000,
        DEFAULT_LOOKBACK_DELAY_HOURS,
        DEFAULT_OFFERING_PERIOD_HOURS,
    )
    .unwrap();
    m.worldwide_days.entry(wwd).status().write(target).unwrap();
}

#[test]
fn test_mark_completed_rejects_non_ready_status() {
    with_contract(|m| {
        let wwd = WwdKey::new(20260101);
        setup_wwd_at_status(m, wwd, status::FORMING);
        let err = m.mark_completed(wwd).unwrap_err();
        assert!(err.to_string().contains("COMPLETED"));
        assert_eq!(m.get_status(wwd).unwrap(), status::FORMING);
    });
}

#[test]
fn test_mark_completed_rejects_already_failed_day() {
    with_contract(|m| {
        let wwd = WwdKey::new(20260102);
        setup_wwd_at_status(m, wwd, status::FAILED);
        assert!(m.mark_completed(wwd).is_err());
        assert_eq!(m.get_status(wwd).unwrap(), status::FAILED);
    });
}

#[test]
fn test_mark_completed_allows_ready() {
    with_contract(|m| {
        let wwd = WwdKey::new(20260103);
        setup_wwd_at_status(m, wwd, status::READY);
        m.mark_completed(wwd).unwrap();
        assert_eq!(m.get_status(wwd).unwrap(), status::COMPLETED);
    });
}

#[test]
fn test_mark_failed_rejects_completed_day() {
    with_contract(|m| {
        let wwd = WwdKey::new(20260104);
        setup_wwd_at_status(m, wwd, status::COMPLETED);
        let err = m.mark_failed(wwd).unwrap_err();
        assert!(err.to_string().contains("COMPLETED"));
        assert_eq!(m.get_status(wwd).unwrap(), status::COMPLETED);
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
            m.mark_failed(wwd).unwrap();
            assert_eq!(m.get_status(wwd).unwrap(), status::FAILED);
        }
    });
}

#[test]
fn test_storage_dsl_layout_is_compatible_with_previous_slots() {
    with_contract(|m| {
        assert_eq!(m.bootstrap_end_time.slot(), U256::ZERO);
        assert_eq!(m.worldwide_days.base_slot(), U256::from(1u64));
        assert_eq!(<WorldwideDay as StorageRecord>::SLOTS, 9);
        assert_eq!(m.day_limit_amount.base_slot(), U256::from(10u64));
        assert_eq!(m.day_limit_used.base_slot(), U256::from(11u64));
        assert_eq!(m.active_wwd_count.slot(), U256::from(12u64));
        assert_eq!(m.active_wwds.base_slot(), U256::from(13u64));
        assert_eq!(m.config_oracle_pair_hash.slot(), U256::from(14u64));
        assert_eq!(m.day_limit_exists.base_slot(), U256::from(15u64));
        assert_eq!(m.day_limit_count.slot(), U256::from(16u64));
        assert_eq!(m.day_limit_dates.base_slot(), U256::from(17u64));
    });
}
