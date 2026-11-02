use super::*;
use crate::api;
use outbe_primitives::time::{date_key_to_utc_timestamp, next_date_key, WorldwideDay};

#[test]
fn init_genesis_day_creates_7_day_lookahead_with_bidirectional_links_and_closed_form_limits() {
    with_storage(|storage| {
        let genesis_ts = date_key_to_utc_timestamp(20260920).unwrap() + 12 * 3_600;
        storage.set_block_timestamp(U256::from(genesis_ts)).unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(storage.block_number().unwrap(), genesis_ts, CHAIN_ID),
            storage.clone(),
        );
        crate::commands::init_genesis_day(&ctx).unwrap();

        let active_wwds = TributeFactoryContract::new(storage.clone())
            .pipeline_wwd
            .read_all()
            .unwrap();
        assert_eq!(active_wwds.len(), 7);

        // Verify wwd = 20260920 -> day = 20260924 (and wwd = 20260925 -> day = 20260929)
        let wwd_20 = api::worldwide_day(storage.clone(), WorldwideDay::new(20260920))
            .unwrap()
            .unwrap();
        assert_eq!(wwd_20.day, 20260924);

        let ed_24 = api::emission_day(storage.clone(), 20260924)
            .unwrap()
            .unwrap();
        assert_eq!(ed_24.day, 20260924);
        assert_eq!(ed_24.wwd, WorldwideDay::new(20260920));
        assert_eq!(
            ed_24.metadosis_start,
            date_key_to_utc_timestamp(next_date_key(20260924).expect("date step")).unwrap() + 1
        );
        assert_eq!(ed_24.state, emission_day_state::NONE);
        assert_eq!(ed_24.emission_limit_minor, U256::ZERO);

        let wwd_25 = api::worldwide_day(storage.clone(), WorldwideDay::new(20260925))
            .unwrap()
            .unwrap();
        assert_eq!(wwd_25.day, 20260929);
        let ed_29 = api::emission_day(storage.clone(), 20260929)
            .unwrap()
            .unwrap();
        assert_eq!(ed_29.wwd, WorldwideDay::new(20260925));
        assert_eq!(ed_29.emission_limit_minor, U256::ZERO);

        // Verify all 7 pairs have symmetric bi-directional links
        let mut wwd_key = 20260920;
        for _ in 0..7u32 {
            let wwd = WorldwideDay::new(wwd_key);
            let wwd_proj = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
            let ed_proj = api::emission_day(storage.clone(), wwd_proj.day)
                .unwrap()
                .unwrap();
            assert_eq!(ed_proj.wwd, wwd);
            assert_eq!(ed_proj.emission_limit_minor, U256::ZERO);
            assert_eq!(
                ed_proj.metadosis_start,
                date_key_to_utc_timestamp(next_date_key(ed_proj.day).expect("date step")).unwrap()
                    + 1
            );
            wwd_key = next_date_key(wwd_key).expect("date step");
        }
    });
}

#[test]
fn oip_00335_schedule_places_emission_day_after_offering_wwd_and_resumes_oldest_due_after_halt() {
    with_storage(|storage| {
        let wwd_sep_01 = WorldwideDay::new(20260901);
        let genesis_ts = date_key_to_utc_timestamp(20260901).unwrap() + 12 * 3_600;
        storage.set_block_timestamp(U256::from(genesis_ts)).unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(storage.block_number().unwrap(), genesis_ts, CHAIN_ID),
            storage.clone(),
        );
        crate::commands::init_genesis_day(&ctx).unwrap();

        let wwd_proj = api::worldwide_day(storage.clone(), wwd_sep_01)
            .unwrap()
            .unwrap();
        assert_eq!(
            WorldwideDay::from_timestamp(wwd_proj.offerings_start),
            WorldwideDay::new(20260904)
        );
        assert_eq!(
            wwd_proj.offering_end,
            date_key_to_utc_timestamp(20260905).unwrap() + 12 * 3_600
        );
        assert_eq!(wwd_proj.day, 20260905);

        let expected_metadosis_start = date_key_to_utc_timestamp(20260906).unwrap() + 1;
        assert_eq!(
            expected_metadosis_start,
            wwd_proj.offering_end + 12 * 3_600 + 1
        );

        let ed_05 = api::emission_day(storage.clone(), 20260905)
            .unwrap()
            .unwrap();
        assert_eq!(ed_05.day, 20260905);
        assert_eq!(ed_05.wwd, wwd_sep_01);
        assert_eq!(ed_05.metadosis_start, expected_metadosis_start);
        assert_eq!(ed_05.emission_limit_minor, U256::ZERO);

        // Nested process-ready checks recompute the lease from the provider clock
        // and block. Allocate day 20260905 into Processing so find_due_emission_day selects it.
        crate::commands::dispatch_terminal_remainder_at(
            &ctx,
            U256::from(1_000_000u64),
            date_key_to_utc_timestamp(20260905).unwrap(),
        )
        .unwrap();

        let metadosis = MetadosisContract::new(storage.clone());
        let before_start = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, expected_metadosis_start - 1, CHAIN_ID),
            storage.clone(),
        );
        assert!(
            crate::lifecycle::find_due_emission_day(&metadosis, &before_start)
                .unwrap()
                .is_none()
        );

        let at_start = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(3, expected_metadosis_start, CHAIN_ID),
            storage.clone(),
        );
        let due_at_start = crate::lifecycle::find_due_emission_day(&metadosis, &at_start)
            .unwrap()
            .unwrap();
        assert_eq!(due_at_start.day, 20260905);
        assert_eq!(due_at_start.wwd, wwd_sep_01);

        // Simulate a halt from 5 Sep to 8 Sep 2026: the first block after restart
        // must still select and execute the oldest due emission day (20260905).
        let restart_ts = date_key_to_utc_timestamp(20260908).unwrap() + 3_600;
        arm_reference_price(&storage, restart_ts);
        let restart_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(4, restart_ts, CHAIN_ID),
            storage.clone(),
        );
        let due_after_halt = crate::lifecycle::find_due_emission_day(&metadosis, &restart_ctx)
            .unwrap()
            .unwrap();
        assert_eq!(due_after_halt.day, 20260905);
        assert_eq!(due_after_halt.wwd, wwd_sep_01);

        storage.set_block_timestamp(U256::from(restart_ts)).unwrap();
        let aligned = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(storage.block_number().unwrap(), restart_ts, CHAIN_ID),
            storage.clone(),
        );
        crate::commands::start_metadosis(&aligned).unwrap();
        let completed = api::emission_day(storage.clone(), 20260905)
            .unwrap()
            .unwrap();
        assert_eq!(completed.day, 20260905);
        assert_eq!(completed.wwd, wwd_sep_01);
        assert_eq!(completed.state, emission_day_state::SUCCEEDED);
        let desis = outbe_desis::schema::DesisContract::new(storage);
        let briefed = outbe_primitives::time::EmissionDate::new(20260905);
        assert_eq!(
            desis.auction_stage.read(&briefed).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
        assert_eq!(desis.brief_has_limit.read(&briefed).unwrap(), 0);
    });
}

#[test]
fn first_wwd_for_genesis_splits_3_or_4_empty_emission_days_across_noon_utc() {
    assert_eq!(outbe_chain_constants::MAX_PIPELINE_WWDS, 7);
    assert_eq!(
        outbe_tributefactory::worldwideday::constants::MAX_PIPELINE_WWDS,
        7
    );
    assert_eq!(crate::constants::MAX_PIPELINE_WWDS, 7);
    // Genesis on 2026-10-01 at 11:59 UTC -> first_wwd = 20260930 (G - 1),
    // offering_end on 20261004 (G + 3) -> 3 empty EmissionDays (20261001..=20261003).
    with_storage(|storage| {
        let genesis_before_noon =
            date_key_to_utc_timestamp(20261001).unwrap() + 11 * 3_600 + 59 * 60;
        assert_eq!(
            outbe_tributefactory::worldwideday::first_wwd_for_genesis(genesis_before_noon),
            WorldwideDay::new(20260930)
        );
        storage
            .set_block_timestamp(U256::from(genesis_before_noon))
            .unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                storage.block_number().unwrap(),
                genesis_before_noon,
                CHAIN_ID,
            ),
            storage.clone(),
        );
        crate::commands::init_genesis_day(&ctx).unwrap();

        let first_proj = api::worldwide_day(storage.clone(), WorldwideDay::new(20260930))
            .unwrap()
            .unwrap();
        assert_eq!(first_proj.day, 20261004);
        let mut day = 20261001;
        let mut empty_count = 0u32;
        while day < first_proj.day {
            let shell = outbe_emission::api::ensure_emission_day_shell(storage.clone(), day)
                .expect("empty day shell");
            assert_eq!(shell.wwd, WorldwideDay::new(0));
            empty_count += 1;
            day = next_date_key(day).unwrap();
        }
        assert_eq!(empty_count, 3);

        crate::commands::dispatch_terminal_remainder_at(
            &ctx,
            U256::from(1_000_000u64),
            date_key_to_utc_timestamp(20261001).unwrap(),
        )
        .unwrap();
        let empty_due_ts = date_key_to_utc_timestamp(20261002).unwrap() + 1;
        storage
            .set_block_timestamp(U256::from(empty_due_ts))
            .unwrap();
        let empty_due_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(storage.block_number().unwrap(), empty_due_ts, CHAIN_ID),
            storage.clone(),
        );
        crate::commands::start_metadosis(&empty_due_ctx).unwrap();
        let settled_empty = api::emission_day(storage, 20261001).unwrap().unwrap();
        assert_eq!(settled_empty.wwd, WorldwideDay::new(0));
        assert_eq!(settled_empty.state, emission_day_state::SUCCEEDED);
    });

    // Genesis on 2026-10-01 at 12:01 UTC -> first_wwd = 20261001 (G),
    // offering_end on 20261005 (G + 4) -> 4 empty EmissionDays (20261001..=20261004).
    with_storage(|storage| {
        let genesis_after_noon = date_key_to_utc_timestamp(20261001).unwrap() + 12 * 3_600 + 60;
        assert_eq!(
            outbe_tributefactory::worldwideday::first_wwd_for_genesis(genesis_after_noon),
            WorldwideDay::new(20261001)
        );
        storage
            .set_block_timestamp(U256::from(genesis_after_noon))
            .unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                storage.block_number().unwrap(),
                genesis_after_noon,
                CHAIN_ID,
            ),
            storage.clone(),
        );
        crate::commands::init_genesis_day(&ctx).unwrap();

        let first_proj = api::worldwide_day(storage.clone(), WorldwideDay::new(20261001))
            .unwrap()
            .unwrap();
        assert_eq!(first_proj.day, 20261005);
        let mut day = 20261001;
        let mut empty_count = 0u32;
        while day < first_proj.day {
            let shell = outbe_emission::api::ensure_emission_day_shell(storage.clone(), day)
                .expect("empty day shell");
            assert_eq!(shell.wwd, WorldwideDay::new(0));
            empty_count += 1;
            day = next_date_key(day).unwrap();
        }
        assert_eq!(empty_count, 4);

        // Verify catch-up after a multi-day gap still creates every unclosed WWD
        // needed by the upcoming emission days (20261008..=20261010 -> E = 20261012..=20261014).
        let catchup_ts = date_key_to_utc_timestamp(20261011).unwrap() + 12 * 3_600;
        storage.set_block_timestamp(U256::from(catchup_ts)).unwrap();
        let mut catchup_block =
            BlockContext::empty_for_tests(storage.block_number().unwrap(), catchup_ts, CHAIN_ID);
        catchup_block.genesis_timestamp = genesis_after_noon;
        let catchup_ctx = BlockRuntimeContext::new(catchup_block, storage.clone());
        crate::commands::advance_active_worldwide_days(&catchup_ctx).unwrap();
        crate::commands::init_genesis_day(&catchup_ctx).unwrap();
        for wwd_key in 20261008..=20261017 {
            let proj = api::worldwide_day(storage.clone(), WorldwideDay::new(wwd_key))
                .unwrap()
                .expect("catch-up and lookahead WWD must exist");
            let ed = api::emission_day(storage.clone(), proj.day)
                .unwrap()
                .expect("linked emission day must exist");
            assert_eq!(ed.wwd, WorldwideDay::new(wwd_key));
        }
    });
}

#[test]
fn wwd_lifecycle_gates_activation_on_forming_vwap_and_seals_on_close() {
    with_storage(|storage| {
        let wwd = WorldwideDay::new(20261001);
        let genesis_ts = date_key_to_utc_timestamp(20261001).unwrap() + 12 * 3_600;
        storage.set_block_timestamp(U256::from(genesis_ts)).unwrap();
        let bn = storage.block_number().unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(bn, genesis_ts, CHAIN_ID),
            storage.clone(),
        );
        crate::commands::init_genesis_day(&ctx).unwrap();

        let tribute = TributeContract::new(storage.clone());
        let factory = TributeFactoryContract::new(storage.clone());
        let proj = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
        assert_eq!(
            proj.state,
            outbe_tributefactory::worldwideday::wwd_state::PENDING
        );
        // Pending WWD is NOT sealed at creation time (P2.3d) and is not offering (P2.3c).
        assert!(!tribute.is_day_sealed(wwd).unwrap());
        assert!(!api::is_offering_day(storage.clone(), wwd).unwrap());

        // Advance to forming_end with NO oracle data: forming_snapshot_finalized is set,
        // WWD stays Pending (not closed early, P2.2g).
        storage
            .set_block_timestamp(U256::from(proj.forming_end))
            .unwrap();
        let at_forming_end = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(bn, proj.forming_end, CHAIN_ID),
            storage.clone(),
        );
        crate::commands::advance_active_worldwide_days(&at_forming_end).unwrap();
        assert!(factory.forming_snapshot_finalized.read(&wwd).unwrap());
        let after_forming = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
        assert_eq!(
            after_forming.state,
            outbe_tributefactory::worldwideday::wwd_state::PENDING
        );

        // Advance to offerings_start: without forming VWAP, WWD stays Pending and is_offering_day is false (P2.2h, Q8.1).
        storage
            .set_block_timestamp(U256::from(proj.offerings_start))
            .unwrap();
        let at_offering_start = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(bn, proj.offerings_start, CHAIN_ID),
            storage.clone(),
        );
        crate::commands::advance_active_worldwide_days(&at_offering_start).unwrap();
        let at_offering = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
        assert_eq!(
            at_offering.state,
            outbe_tributefactory::worldwideday::wwd_state::PENDING
        );
        assert!(!api::is_offering_day(storage.clone(), wwd).unwrap());
        assert!(!factory.active_wwd.read_all().unwrap().contains(&wwd));

        // Advance to offering_end: WWD transitions to Closed, is removed from pipeline_wwd & active_wwd (P2.2c),
        // and is sealed in outbe-tribute (P2.3d).
        storage
            .set_block_timestamp(U256::from(proj.offering_end))
            .unwrap();
        let at_offering_end = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(bn, proj.offering_end, CHAIN_ID),
            storage.clone(),
        );
        crate::commands::advance_active_worldwide_days(&at_offering_end).unwrap();
        let closed = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
        assert_eq!(
            closed.state,
            outbe_tributefactory::worldwideday::wwd_state::CLOSED
        );
        assert!(!factory.pipeline_wwd.read_all().unwrap().contains(&wwd));
        assert!(!factory.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(tribute.is_day_sealed(wwd).unwrap());
    });
}
