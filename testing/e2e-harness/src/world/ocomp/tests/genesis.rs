use super::*;

#[cfg(feature = "ocomp-integration")]
#[test]
fn measurement_fork_install_arms_genesis_without_a_synthetic_update() {
    let topology = topology();
    prepare_measurement_genesis_fixture(&topology);
    let prepared = topology.prepare_measurement_fork_install().unwrap();
    assert_eq!(
        prepared.install.classification,
        OcompForkInstallClassification::Measurement
    );
    assert_eq!(
        prepared.install.activation_height,
        OCOMP_MEASUREMENT_ACTIVATION_HEIGHT
    );
    assert_eq!(
        prepared.install.founder_registrations.len(),
        topology.domains.len()
    );
    for (index, registration) in prepared.install.founder_registrations.iter().enumerate() {
        assert_eq!(
            registration.core.validator_identity_hash,
            validator_identity_hash_v1(
                Address::with_last_byte(u8::try_from(index).unwrap() + 1),
                &[u8::try_from(index).unwrap() + 11; 48],
            )
            .unwrap()
        );
        assert_eq!(
            registration.core.ocomp_public_key_sec1.as_slice(),
            measurement_signing_key(u8::try_from(index).unwrap())
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
        );
    }
    assert_eq!(
        format!("{METADOSIS_STORAGE_LAYOUT_V1_HASH:#x}"),
        METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX
    );

    let chain_spec = parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json")).unwrap();
    let loaded = outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&chain_spec).unwrap();
    assert_eq!(loaded.as_ref(), &prepared.install);
    assert_eq!(
        loaded
            .install_hash(&outbe_ocomp_protocol::profile::poc_schema_limits())
            .unwrap(),
        prepared.install_hash
    );

    let genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
            .unwrap();
    let alloc = genesis["alloc"].as_object().unwrap();
    assert!(
        find_alloc_address_key(alloc, outbe_primitives::addresses::UPDATE_ADDRESS)
            .unwrap()
            .is_none(),
        "measurement genesis must not schedule a generic Update for OCOMP"
    );

    let bundles = topology
        .validator_indices()
        .unwrap()
        .into_iter()
        .map(|index| {
            std::fs::read(
                topology
                    .domain_root(index)
                    .unwrap()
                    .join("protocol-bundle-v1.ocb1"),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();

    assert!(bundles.iter().all(|bundle| bundle == &bundles[0]));
    for index in topology.validator_indices().unwrap() {
        let key = std::fs::read_to_string(
            topology
                .domain_root(index)
                .unwrap()
                .join("ocomp-key-v1.hex"),
        )
        .unwrap();
        let signer =
            SigningKey::from_bytes((&hex::decode(key.trim()).unwrap()[..]).into()).unwrap();
        assert_eq!(
            signer.verifying_key().to_encoded_point(true).as_bytes(),
            measurement_signing_key(index)
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
        );
    }
    assert_eq!(
        topology.prepare_measurement_fork_install().unwrap(),
        prepared
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn cross_currency_tribute_fixture_persists_exact_oracle_inputs() {
    let topology = topology();
    prepare_public_measurement_genesis_fixture(&topology);
    let worldwide_day =
        WorldwideDay::new(topology.prepare_cross_currency_tribute_fixture().unwrap());
    let genesis: serde_json::Value =
        serde_json::from_slice(&fs::read(topology.cfg.dir.join("genesis.json")).unwrap()).unwrap();
    let alloc = genesis["alloc"].as_object().unwrap();
    let oracle_key = find_alloc_address_key(alloc, ORACLE_ADDRESS)
        .unwrap()
        .unwrap();
    let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
    for (slot, value) in alloc[&oracle_key]["storage"].as_object().unwrap() {
        provider.storage.insert(
            (ORACLE_ADDRESS, parse_hex_word(slot).unwrap()),
            parse_storage_word(value).unwrap(),
        );
    }
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            outbe_oracle::api::tribute_pricing_inputs(storage, 949, 978, worldwide_day)
                .unwrap()
                .unwrap(),
            outbe_oracle::api::TributePricingInputs {
                issuance_wwd_vwap_minor: U256::from(10_250_000_u64),
                reference_wwd_vwap_minor: U256::from(250_000_u64),
                reference_scurve_minor: U256::from(320_000_u64),
            }
        );
    });
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn cross_currency_tribute_fixture_preserves_a_longer_configured_offering() {
    let topology = topology();
    prepare_public_measurement_genesis_fixture(&topology);
    let genesis_path = topology.cfg.dir.join("genesis.json");
    let mut genesis: serde_json::Value =
        serde_json::from_slice(&fs::read(&genesis_path).unwrap()).unwrap();
    genesis["config"][GENESIS_CONFIG_KEY]["metadosis"]["offeringPeriodSeconds"] =
        serde_json::json!(1_800);
    replace_json_atomically(&genesis_path, &genesis).unwrap();

    topology.prepare_cross_currency_tribute_fixture().unwrap();

    let genesis: serde_json::Value =
        serde_json::from_slice(&fs::read(&genesis_path).unwrap()).unwrap();
    let genesis_timestamp =
        u64::try_from(parse_hex_word(genesis["timestamp"].as_str().unwrap()).unwrap()).unwrap();
    let alloc = genesis["alloc"].as_object().unwrap();
    let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
        .unwrap()
        .unwrap();
    let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
    for (slot, value) in alloc[&metadosis_key]["storage"].as_object().unwrap() {
        provider.storage.insert(
            (METADOSIS_ADDRESS, parse_hex_word(slot).unwrap()),
            parse_storage_word(value).unwrap(),
        );
    }
    StorageHandle::enter(&mut provider, |storage| {
        let days = outbe_metadosis::api::offering_worldwide_days(storage.clone()).unwrap();
        assert_eq!(days.len(), 1);
        let day = outbe_metadosis::api::worldwide_day(storage, days[0])
            .unwrap()
            .unwrap();
        assert_eq!(day.offering_end - genesis_timestamp, 1_800);
    });
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn public_measurement_schedule_materializes_omitted_empty_metadosis_storage() {
    let topology = topology();
    prepare_public_measurement_genesis_fixture(&topology);
    let genesis_path = topology.cfg.dir.join("genesis.json");
    let mut genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
    let alloc = genesis["alloc"].as_object_mut().unwrap();
    let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
        .unwrap()
        .unwrap();
    alloc[&metadosis_key]
        .as_object_mut()
        .unwrap()
        .remove("storage");
    std::fs::write(&genesis_path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();

    topology.prepare_public_measurement_fork_install().unwrap();

    let materialized: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
    let storage = materialized["alloc"][&metadosis_key]["storage"]
        .as_object()
        .expect("the public measurement fixture must materialize Metadosis storage");
    assert!(
        !storage.is_empty(),
        "the scheduled public measurement day must persist Metadosis state"
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn public_measurement_schedule_seeds_consistent_green_day_and_oracle_vwaps() {
    let topology = topology();
    prepare_public_measurement_genesis_fixture(&topology);
    let prepared = topology.prepare_public_measurement_fork_install().unwrap();
    let genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
            .unwrap();
    let genesis_timestamp =
        u64::try_from(parse_hex_word(genesis["timestamp"].as_str().unwrap()).unwrap()).unwrap();
    let alloc = genesis["alloc"].as_object().unwrap();
    let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
        .unwrap()
        .unwrap();
    let oracle_key = find_alloc_address_key(alloc, outbe_primitives::addresses::ORACLE_ADDRESS)
        .unwrap()
        .unwrap();
    let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
    for (address, account_key) in [
        (METADOSIS_ADDRESS, metadosis_key),
        (outbe_primitives::addresses::ORACLE_ADDRESS, oracle_key),
    ] {
        for (slot, value) in alloc[&account_key]["storage"].as_object().unwrap() {
            provider.storage.insert(
                (address, parse_hex_word(slot).unwrap()),
                parse_storage_word(value).unwrap(),
            );
        }
    }
    StorageHandle::enter(&mut provider, |storage| {
        let days = outbe_metadosis::api::offering_worldwide_days(storage.clone()).unwrap();
        assert_eq!(days.len(), 1);
        let day = outbe_metadosis::api::worldwide_day(storage.clone(), days[0])
            .unwrap()
            .unwrap();
        assert_eq!(
            day.worldwide_day,
            WorldwideDay::from_timestamp(genesis_timestamp)
        );
        assert_eq!(day.status, WwdStatus::Offering);
        assert_eq!(day.day_type, WwdDayType::Green);
        assert_eq!(
            day.offering_end - genesis_timestamp,
            OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS,
            "the hardware-SGX fixture must retain the frozen public offering allowance"
        );
        assert_eq!(day.scheduled_process_time, day.offering_end);
        assert!(day.metadosis_limit_amount > U256::ZERO);
        assert!(day.previous_vwap > U256::ZERO);
        assert!(day.current_vwap > day.previous_vwap);
        assert_eq!(
            outbe_oracle::api::day_type_pair_vwap(
                storage.clone(),
                day.worldwide_day.previous_date_key(),
            )
            .unwrap(),
            Some(day.previous_vwap)
        );
        assert_eq!(
            outbe_oracle::api::day_type_pair_vwap(storage.clone(), day.worldwide_day).unwrap(),
            Some(day.current_vwap)
        );
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let (_, pair_index) = outbe_oracle::api::require_coen_pair(storage.clone(), 840).unwrap();
        let vwap = outbe_oracle::api::get_worldwide_day_vwap_for_pair(
            storage.clone(),
            day.worldwide_day,
            pair_index,
        )
        .unwrap()
        .unwrap();
        let scurve = outbe_oracle::api::get_max_active_scurve_value(
            storage.clone(),
            day.worldwide_day,
            pair,
        )
        .unwrap();
        let entry_price = vwap.max(scurve);
        assert!(entry_price > U256::ZERO);
        assert!(
            scurve > vwap,
            "the first OCOMP scenario must retain a higher active S-curve so Lysis proves it uses WWD VWAP only"
        );
        let current_rate = outbe_oracle::api::coen_rate_for(storage, 840).unwrap();
        assert_eq!(current_rate, entry_price * U256::from(2));
        let scale = outbe_primitives::units::SCALE_1E6_U256;
        assert_eq!(day.metadosis_limit_amount, U256::from(500) * scale);
        assert_eq!(OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE, "2");
        assert_eq!(OCOMP_PUBLIC_TRIBUTE_AMOUNT_MICRO, "0");
        let amount_base = U256::from(
            OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE
                .parse::<u64>()
                .expect("canonical amount_base"),
        );
        let amount_micro = U256::from(
            OCOMP_PUBLIC_TRIBUTE_AMOUNT_MICRO
                .parse::<u64>()
                .expect("canonical amount_micro"),
        );
        assert!(amount_micro < scale);
        let issuance = amount_base
            .checked_mul(scale)
            .and_then(|value| value.checked_add(amount_micro))
            .expect("canonical Tribute amount");
        assert_eq!(issuance, U256::from(2_000_000));
        let nominal = issuance * scale / day.current_vwap;
        for (population, expected_fraction, expected_load, expected_cost) in [
            (10_u64, 50_u64, 50_000_000_u64, 100_u64),
            (257_u64, 1_u64, 1_000_000_u64, 2_u64),
        ] {
            let total_nominal = nominal * U256::from(population);
            let allocation =
                (total_nominal * U256::from(32) / U256::from(100)).min(day.metadosis_limit_amount);
            let fraction = allocation * scale / total_nominal;
            let gratis_load_minor = nominal * fraction / scale;
            assert_eq!(
                fraction,
                U256::from(expected_fraction),
                "the {population}-Tribute fixture must preserve the canonical capped allocation fraction"
            );
            assert_eq!(
                gratis_load_minor,
                U256::from(expected_load),
                "the {population}-Tribute fixture must preserve its exact per-Tribute Gratis load"
            );
            assert_eq!(
                day.current_vwap * gratis_load_minor / scale,
                U256::from(expected_cost),
                "the {population}-Tribute fixture must produce a paid Nod under the canonical WWD-VWAP Lysis price"
            );
        }
    });
    assert_eq!(
        prepared.install.request_profile.genesis_hash,
        parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json"))
            .unwrap()
            .genesis_hash()
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn public_recovery_fixture_seeds_two_empty_ordered_days() {
    let topology = topology();
    prepare_public_measurement_genesis_fixture(&topology);
    let prepared = topology.prepare_public_recovery_fork_install().unwrap();
    let first_worldwide_day = prepared
        .public_worldwide_day
        .expect("public recovery fixture first WWD");
    let second_worldwide_day = WorldwideDay::from_timestamp(
        first_worldwide_day
            .start_timestamp()
            .checked_add(86_400)
            .unwrap(),
    );

    let genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
            .unwrap();
    let alloc = genesis["alloc"].as_object().unwrap();
    let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
    for (address, label) in [
        (METADOSIS_ADDRESS, "Metadosis"),
        (ORACLE_ADDRESS, "Oracle"),
        (TRIBUTE_ADDRESS, "Tribute"),
    ] {
        let account_key = find_alloc_address_key(alloc, address)
            .unwrap()
            .unwrap_or_else(|| panic!("recovery genesis omitted {label}"));
        for (slot, value) in alloc[&account_key]["storage"].as_object().unwrap() {
            provider.storage.insert(
                (address, parse_hex_word(slot).unwrap()),
                parse_storage_word(value).unwrap(),
            );
        }
    }

    StorageHandle::enter(&mut provider, |storage| {
        let days = outbe_metadosis::api::worldwide_days(storage.clone()).unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].worldwide_day, first_worldwide_day);
        assert_eq!(days[1].worldwide_day, second_worldwide_day);
        assert!(
            days.iter().all(|day| day.status == WwdStatus::Offering),
            "both recovery WWDs must start in OFFERING: {days:?}"
        );
        assert!(
            days[1].scheduled_process_time > days[0].scheduled_process_time,
            "Job B must be scheduled strictly after Job A"
        );
        assert!(days[1].metadosis_limit_amount > U256::ZERO);
        assert_ne!(days[1].day_type, WwdDayType::Unknown);
        assert!(
            outbe_oracle::api::day_type_pair_vwap(storage.clone(), second_worldwide_day)
                .unwrap()
                .is_some_and(|price| !price.is_zero()),
            "Job B WWD must have a nonzero Oracle VWAP"
        );

        let totals = outbe_tribute::TributeContract::new(storage.clone())
            .get_day_totals(second_worldwide_day)
            .unwrap();
        assert!(totals.initialized);
        assert!(!totals.is_sealed);
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_amount, U256::ZERO);
    });
    assert_eq!(
        prepared.install.request_profile.genesis_hash,
        parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json"))
            .unwrap()
            .genesis_hash()
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn dynamic_membership_fixture_schedules_two_distinct_public_jobs() {
    let topology = topology();
    prepare_public_measurement_genesis_fixture_with_vote_window(
        &topology,
        OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS,
    );
    let genesis_path = topology.cfg.dir.join("genesis.json");
    let mut configured: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
    configured["config"][outbe_node::ocomp::fork::EPOCH_LENGTH_BLOCKS_GENESIS_KEY] =
        serde_json::json!(OCOMP_TEST_EPOCH_LENGTH_BLOCKS);
    configured["config"]["dkgPrepareWindowBlocks"] =
        serde_json::json!(OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS);
    std::fs::write(
        &genesis_path,
        serde_json::to_vec_pretty(&configured).unwrap(),
    )
    .unwrap();

    let prepared = topology.prepare_dynamic_membership_fork_install().unwrap();

    let genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
            .unwrap();
    assert_eq!(
        genesis["config"][outbe_node::ocomp::fork::EPOCH_LENGTH_BLOCKS_GENESIS_KEY],
        serde_json::json!(OCOMP_TEST_EPOCH_LENGTH_BLOCKS)
    );
    let alloc = genesis["alloc"].as_object().unwrap();
    let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
        .unwrap()
        .unwrap();
    let tribute_key = find_alloc_address_key(alloc, TRIBUTE_ADDRESS)
        .unwrap()
        .unwrap();
    let oracle_key = find_alloc_address_key(alloc, ORACLE_ADDRESS)
        .unwrap()
        .unwrap();
    let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
    for (address, account_key) in [
        (METADOSIS_ADDRESS, metadosis_key),
        (ORACLE_ADDRESS, oracle_key),
        (TRIBUTE_ADDRESS, tribute_key),
    ] {
        for (slot, value) in alloc[&account_key]["storage"].as_object().unwrap() {
            provider.storage.insert(
                (address, parse_hex_word(slot).unwrap()),
                parse_storage_word(value).unwrap(),
            );
        }
    }
    StorageHandle::enter(&mut provider, |storage| {
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        assert_eq!(
            oracle.config_vote_period.read().unwrap(),
            E2E_ORACLE_VOTE_PERIOD_BLOCKS,
            "dynamic OCOMP must use the shared E2E Oracle voting window"
        );
        let days = outbe_metadosis::api::worldwide_days(storage.clone()).unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(
            days.iter()
                .map(|day| (day.worldwide_day, day.status, day.scheduled_process_time))
                .collect::<Vec<_>>(),
            vec![
                (
                    prepared.first_worldwide_day,
                    WwdStatus::Offering,
                    prepared.first_processing_time,
                ),
                (
                    prepared.second_worldwide_day,
                    WwdStatus::Offering,
                    prepared.second_processing_time,
                ),
            ]
        );
        let second_totals = outbe_tribute::TributeContract::new(storage.clone())
            .get_day_totals(prepared.second_worldwide_day)
            .unwrap();
        assert!(second_totals.initialized);
        assert!(!second_totals.is_sealed);
        for worldwide_day in [prepared.first_worldwide_day, prepared.second_worldwide_day] {
            assert!(
                outbe_oracle::api::day_type_pair_vwap(storage.clone(), worldwide_day)
                    .unwrap()
                    .is_some_and(|price| !price.is_zero()),
                "dynamic OCOMP fixture must price COEN/840 for {worldwide_day}"
            );
        }
    });
    assert!(prepared.first_processing_time < prepared.second_processing_time);
    let genesis_timestamp = genesis["timestamp"]
        .as_str()
        .map(parse_hex_word)
        .transpose()
        .unwrap()
        .and_then(|timestamp| u64::try_from(timestamp).ok())
        .expect("dynamic fixture genesis timestamp");
    let expected_controlled_daily_cycle = genesis_timestamp
        .checked_div(86_400)
        .and_then(|day| day.checked_add(2))
        .and_then(|day| day.checked_mul(86_400))
        .and_then(|midnight| midnight.checked_add(1))
        .expect("controlled dynamic Job B daily-cycle boundary");
    assert_eq!(
        prepared.second_processing_time, expected_controlled_daily_cycle,
        "dynamic Job B must stay Scheduled until the controlled daily Cycle after membership activation"
    );
    assert_eq!(prepared.fork.install.founder_registrations.len(), 4);
    assert_eq!(
        prepared
            .fork
            .install
            .request_profile
            .capacity_profile
            .result_deadline_blocks,
        OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS,
        "dynamic OCOMP fixture must use the immutable genesis vote window"
    );
    let chain_spec = parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json")).unwrap();
    outbe_node::ocomp::fork::load_ocomp_fork_install(&chain_spec)
        .unwrap()
        .expect("dynamic membership fixture is startup-valid");
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn dynamic_membership_fixture_refuses_a_post_seed_epoch_rewrite() {
    let topology = topology();
    prepare_public_measurement_genesis_fixture(&topology);

    let error = topology
        .prepare_dynamic_membership_fork_install()
        .unwrap_err();

    assert!(error
        .to_string()
        .contains("must be configured before ValidatorSet genesis is seeded"));
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn public_capacity_fixture_funds_every_distinct_tribute_owner_before_genesis_is_bound() {
    const TRIBUTE_COUNT: usize = 257;

    let topology = topology();
    prepare_public_measurement_genesis_fixture(&topology);
    let (prepared, private_keys) = topology
        .prepare_public_capacity_fork_install(TRIBUTE_COUNT)
        .unwrap();

    assert_eq!(private_keys.len(), TRIBUTE_COUNT);
    assert_eq!(
        private_keys
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        TRIBUTE_COUNT
    );

    let genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
            .unwrap();
    let genesis_timestamp =
        u64::try_from(parse_hex_word(genesis["timestamp"].as_str().unwrap()).unwrap()).unwrap();
    let alloc = genesis["alloc"].as_object().unwrap();
    for private_key in private_keys {
        let owner = crate::internal::eth::address_of(&private_key).unwrap();
        let alloc_key = find_alloc_address_key(alloc, owner)
            .unwrap()
            .expect("capacity Tribute owner is funded in base genesis");
        assert!(
            parse_hex_word(alloc[&alloc_key]["balance"].as_str().unwrap()).unwrap() > U256::ZERO
        );
    }
    let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
        .unwrap()
        .unwrap();
    let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
    for (slot, value) in alloc[&metadosis_key]["storage"].as_object().unwrap() {
        provider.storage.insert(
            (METADOSIS_ADDRESS, parse_hex_word(slot).unwrap()),
            parse_storage_word(value).unwrap(),
        );
    }
    StorageHandle::enter(&mut provider, |storage| {
        let days = outbe_metadosis::api::offering_worldwide_days(storage.clone()).unwrap();
        assert_eq!(days, vec![WorldwideDay::from_timestamp(genesis_timestamp)]);
        assert_eq!(prepared.public_worldwide_day, Some(days[0]));
        let day = outbe_metadosis::api::worldwide_day(storage, days[0])
            .unwrap()
            .unwrap();
        assert_eq!(day.status, WwdStatus::Offering);
        assert_eq!(day.day_type, WwdDayType::Green);
        assert!(day.metadosis_limit_amount > U256::ZERO);
        assert_eq!(
            day.offering_end - genesis_timestamp,
            3_600,
            "the 257-Tribute real-SGX capacity population needs the full bounded one-hour offering window"
        );
        assert_eq!(
            day.offering_end,
            genesis_timestamp + OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS
        );
        assert_eq!(day.scheduled_process_time, day.offering_end);
    });
    assert_eq!(
        prepared.install.request_profile.genesis_hash,
        parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json"))
            .unwrap()
            .genesis_hash()
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn fresh_metadosis_capacity_fixture_materializes_omitted_storage() {
    let topology = topology();
    prepare_public_measurement_genesis_fixture(&topology);
    let genesis_path = topology.cfg.dir.join("genesis.json");
    let mut genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
    let alloc = genesis["alloc"].as_object_mut().unwrap();
    let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
        .unwrap()
        .unwrap();
    alloc[&metadosis_key]
        .as_object_mut()
        .unwrap()
        .remove("storage");
    std::fs::write(&genesis_path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();

    let (prepared, private_keys) = topology
        .prepare_fresh_metadosis_capacity_fork_install(3)
        .unwrap();
    assert_eq!(private_keys.len(), 3);

    let genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
            .unwrap();
    let alloc = genesis["alloc"].as_object().unwrap();
    let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
        .unwrap()
        .unwrap();
    let oracle_key = find_alloc_address_key(alloc, ORACLE_ADDRESS)
        .unwrap()
        .unwrap();
    let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
    for (slot, value) in alloc[&metadosis_key]["storage"].as_object().unwrap() {
        provider.storage.insert(
            (METADOSIS_ADDRESS, parse_hex_word(slot).unwrap()),
            parse_storage_word(value).unwrap(),
        );
    }
    for (slot, value) in alloc[&oracle_key]["storage"].as_object().unwrap() {
        provider.storage.insert(
            (ORACLE_ADDRESS, parse_hex_word(slot).unwrap()),
            parse_storage_word(value).unwrap(),
        );
    }
    let seeded = crate::world::localnet::worldwide_day()
        .parse::<WorldwideDay>()
        .unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        assert_eq!(
            oracle.config_vote_period.read().unwrap(),
            E2E_ORACLE_VOTE_PERIOD_BLOCKS
        );
        assert!(
            outbe_metadosis::test_support::fresh_devnet_sentinel_is_pristine(
                storage.clone(),
                seeded,
            )
            .unwrap()
        );
        let start = seeded.start_timestamp();
        let end = start + outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS;
        assert!(
            outbe_oracle::api::store_worldwide_day_vwap_snapshot(
                storage.clone(),
                seeded,
                start,
                end,
            )
            .unwrap(),
            "fresh fixture must let production form an exact WWD VWAP"
        );
        let inputs = outbe_oracle::api::tribute_pricing_inputs(storage, 840, 840, seeded)
            .unwrap()
            .expect("fresh fixture must price the ordinary USD Tribute after formation");
        assert!(inputs.issuance_wwd_vwap_minor > U256::ZERO);
        assert!(inputs.reference_scurve_minor > inputs.reference_wwd_vwap_minor);
    });
    assert_eq!(
        prepared.install.activation_height,
        OCOMP_MEASUREMENT_ACTIVATION_HEIGHT
    );
    assert_eq!(
        prepared.install.request_profile.genesis_hash,
        parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json"))
            .unwrap()
            .genesis_hash()
    );
}
