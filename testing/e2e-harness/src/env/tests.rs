use super::*;
use std::path::Path;

#[test]
fn paynote_main_and_capacity_have_separate_tags_and_timeouts() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/gem.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .expect("parse GEM features");
    let scenarios: Vec<_> = feature
        .scenarios
        .iter()
        .filter(|s| has_tag(&feature, s, "paynote-capacity"))
        .collect();
    assert_eq!(scenarios.len(), 1);
    let scenario = scenarios[0];
    let main_scenarios: Vec<_> = feature
        .scenarios
        .iter()
        .filter(|s| has_tag(&feature, s, "paynote-main"))
        .collect();
    assert_eq!(main_scenarios.len(), 1);
    let main_scenario = main_scenarios[0];
    assert!(!has_tag(&feature, main_scenario, "paynote-capacity"));
    let mut env = Environment {
        validators: 4,
        all: true,
        ..Environment::default()
    };
    assert_eq!(decide(&feature, scenario, &env), Decision::Run);
    assert_eq!(decide(&feature, main_scenario, &env), Decision::Run);
    assert_eq!(scenario_timeout_secs(&feature, scenario, &env), 21_600);
    assert_eq!(scenario_timeout_secs(&feature, main_scenario, &env), 3_600);
    let ordinary = feature
        .scenarios
        .iter()
        .find(|s| !has_tag(&feature, s, "paynote-capacity"))
        .expect("ordinary GEM lifecycle");
    assert_eq!(scenario_timeout_secs(&feature, ordinary, &env), 3_600);
    if cfg!(feature = "ocomp-integration") {
        assert_eq!(unmet(&feature, scenario, &env), None);
        assert_registered_steps(&feature, scenario);
        assert_eq!(unmet(&feature, main_scenario, &env), None);
        assert_registered_steps(&feature, main_scenario);
    } else {
        assert!(unmet(&feature, scenario, &env)
            .expect("build requirement")
            .contains("ocomp-integration"));
    }
    for seconds in [60, 3_600, 30_000] {
        env.scenario_timeout_secs = Some(seconds);
        assert_eq!(scenario_timeout_secs(&feature, scenario, &env), seconds);
        assert_eq!(
            scenario_timeout_secs(&feature, main_scenario, &env),
            seconds
        );
        assert_eq!(scenario_timeout_secs(&feature, ordinary, &env), seconds);
    }
}
#[cfg(feature = "ocomp-integration")]
#[test]
fn offchain_storage_network_scenario_is_registered_for_rocksdb() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/offchain_storage.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .unwrap();
    let scenario = &feature.scenarios[0];
    for tee_mode in [TeeMode::SgxNoAttest, TeeMode::Real] {
        let env = Environment {
            tee_mode,
            validators: 4,
            sudo: true,
            ..Environment::default()
        };
        assert_eq!(decide(&feature, scenario, &env), Decision::Run);
    }
    assert_registered_steps(&feature, scenario);
}

#[test]
fn projection_backend_is_explicit_and_external_mongo_uri_is_rejected() {
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        env: EnvCli,
    }
    assert_eq!(
        Cli::try_parse_from(["e2e", "--projection-backend", "mongodb"])
            .unwrap()
            .env
            .projection_backend,
        ProjectionBackend::Mongodb
    );
    assert_eq!(
        Cli::try_parse_from(["e2e"]).unwrap().env.projection_backend,
        ProjectionBackend::Rocksdb
    );
    assert!(
        Cli::try_parse_from(["e2e", "--projection-mongodb-uri", "mongodb://127.0.0.1"]).is_err()
    );
    assert_eq!(Cli::try_parse_from(["e2e"]).unwrap().env.validators, 4);
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn native_storage_scenario_runs_only_in_its_explicit_profile_even_with_all() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/offchain_storage_native.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .unwrap();
    let scenario = &feature.scenarios[0];
    for all in [false, true] {
        for tee_mode in [
            TeeMode::Real,
            TeeMode::SgxNoAttest,
            TeeMode::GramineDirect,
            TeeMode::Mock,
            TeeMode::MockNative,
        ] {
            let env = Environment {
                tee_mode,
                all,
                sudo: true,
                validators: 4,
                ..Environment::default()
            };
            assert_eq!(
                matches!(decide(&feature, scenario, &env), Decision::Run),
                tee_mode == TeeMode::MockNative
            );
        }
    }
    assert_registered_steps(&feature, scenario);
}

#[test]
fn settlement_scenarios_declare_their_integration_build_requirement() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/settlement.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .expect("parse settlement feature");
    assert_eq!(feature.scenarios.len(), 2);
    assert!(
        feature.tags.iter().any(|tag| tag == "ocomp"),
        "settlement step registration requires the OCOMP integration build"
    );
    let mut env = Environment {
        tee_mode: TeeMode::SgxNoAttest,
        validators: 4,
        sudo: true,
        ..Environment::default()
    };
    for scenario in &feature.scenarios {
        if cfg!(feature = "ocomp-integration") {
            assert_eq!(unmet(&feature, scenario, &env), None);
            assert_eq!(decide(&feature, scenario, &env), Decision::Run);
        } else {
            assert!(unmet(&feature, scenario, &env)
                .expect("missing build requirement")
                .contains("built without --features ocomp-integration"));
            assert!(matches!(
                decide(&feature, scenario, &env),
                Decision::Skip(_)
            ));
        }
        env.all = true;
        assert_eq!(decide(&feature, scenario, &env), Decision::Run);
        assert_eq!(
            unmet(&feature, scenario, &env).is_some(),
            !cfg!(feature = "ocomp-integration"),
            "--all must expose missing capabilities to the failing before hook"
        );
        env.all = false;
    }
}

fn assert_registered_steps(feature: &Feature, scenario: &Scenario) {
    use cucumber::World as _;

    let steps = crate::world::World::collection();
    for step in feature
        .background
        .iter()
        .flat_map(|background| &background.steps)
        .chain(&scenario.steps)
    {
        assert!(
            steps
                .find(step)
                .unwrap_or_else(|error| panic!("ambiguous step in {}: {error}", scenario.name))
                .is_some(),
            "unregistered {:?} step in {}: {}",
            step.ty,
            scenario.name,
            step.value
        );
    }
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn lifecycle_scenarios_use_only_registered_steps() {
    for file in [
        "nod.feature",
        "gem.feature",
        "intex.feature",
        "credis.feature",
    ] {
        let feature = Feature::parse_path(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("features")
                .join(file),
            cucumber::gherkin::GherkinEnv::default(),
        )
        .expect("parse a lifecycle feature");
        for scenario in &feature.scenarios {
            assert_registered_steps(&feature, scenario);
        }
    }
}

#[test]
fn chained_followers_keep_all_twelve_live_handoff_and_restart_steps() {
    let env = Environment {
        tee_mode: TeeMode::SgxNoAttest,
        validators: 4,
        sudo: true,
        all: true,
        ..Environment::default()
    };
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/fullnode.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .unwrap();
    let scenario = feature
        .scenarios
        .iter()
        .find(|scenario| {
            scenario.name
                == "Chained FullNodes stop on upstream loss and recover through a healthy upstream"
        })
        .unwrap();
    assert_eq!(scenario.steps.len(), 12);
    assert_eq!(unmet(&feature, scenario, &env), None);
    assert_eq!(decide(&feature, scenario, &env), Decision::Run);
    assert_registered_steps(&feature, scenario);
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn worker_outage_precedes_independent_exports_and_includes_independent_recovery() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/ocomp.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .unwrap();
    let scenario = feature
        .scenarios
        .iter()
        .find(|scenario| {
            scenario.name == "Complete worker outage preserves exports and cannot halt consensus"
        })
        .unwrap();
    assert_eq!(scenario.steps.len(), 11);
    assert_eq!(scenario.steps[5].value, "all four OCOMP workers stop before voting opens and exporters independently materialize the public JobIntent");
    assert_eq!(
        scenario.steps[10].value,
        "the independent OCOMP job completes on every validator"
    );
    assert_registered_steps(&feature, scenario);
}

#[test]
fn active_pending_validator_restart_is_selected_with_all_eight_registered_steps() {
    let env = Environment {
        tee_mode: TeeMode::SgxNoAttest,
        validators: 4,
        sudo: true,
        all: true,
        ..Environment::default()
    };
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/txpool_eviction.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .unwrap();
    let selected: Vec<_> = feature
        .scenarios
        .iter()
        .filter(|scenario| has_tag(&feature, scenario, "pending-validator-restart"))
        .collect();
    assert_eq!(selected.len(), 1);
    let scenario = selected[0];
    assert_eq!(scenario.steps.len(), 8);
    assert_eq!(unmet(&feature, scenario, &env), None);
    assert_eq!(decide(&feature, scenario, &env), Decision::Run);
    assert_registered_steps(&feature, scenario);
}

#[test]
fn ordinary_oracle_and_zerofee_rollover_steps_remain_available_without_integration() {
    let env = Environment {
        tee_mode: TeeMode::SgxNoAttest,
        validators: 4,
        sudo: true,
        ..Environment::default()
    };
    let mut checked = Vec::new();
    for file in ["price_oracle.feature", "zerofee.feature"] {
        let feature = Feature::parse_path(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("features")
                .join(file),
            cucumber::gherkin::GherkinEnv::default(),
        )
        .expect("parse ordinary feeder-dependent fixture");
        for scenario in &feature.scenarios {
            if has_tag(&feature, scenario, "price-oracle")
                || has_tag(&feature, scenario, "pfs-007-12")
            {
                assert_eq!(unmet(&feature, scenario, &env), None);
                assert_eq!(decide(&feature, scenario, &env), Decision::Run);
                assert!(!has_tag(&feature, scenario, "ocomp"));
                assert_registered_steps(&feature, scenario);
                checked.push(scenario.name.clone());
            }
        }
    }
    assert_eq!(
        checked,
        [
            "Per-pair quorum survives a sub-quorum cross intersection",
            "Exhausted quota resets lazily across the worldwide-day boundary",
        ]
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn settlement_and_nod_redemption_keep_all_three_executable_scenarios() {
    let env = Environment {
        tee_mode: TeeMode::SgxNoAttest,
        validators: 4,
        sudo: true,
        all: true,
        ..Environment::default()
    };
    let mut checked = Vec::new();
    for file in ["settlement.feature", "ocomp.feature"] {
        let feature = Feature::parse_path(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("features")
                .join(file),
            cucumber::gherkin::GherkinEnv::default(),
        )
        .expect("parse settlement integration fixture");
        for scenario in &feature.scenarios {
            if has_tag(&feature, scenario, "settlement")
                || has_tag(&feature, scenario, "nod-settlement")
            {
                assert_eq!(unmet(&feature, scenario, &env), None);
                assert_eq!(decide(&feature, scenario, &env), Decision::Run);
                assert_registered_steps(&feature, scenario);
                checked.push(scenario.name.clone());
            }
        }
    }
    assert_eq!(checked, [
            "A zero-balance validator redeems its reward Gem through ZeroFee",
            "A stale Oracle rate defers validator Gem delivery and later recovers",
            "A public Tribute completes real OCOMP, FullNode verification, NOD, replay, and contributor payout",
        ]);
}

#[test]
fn gramine_direct_uses_the_production_enclave_without_sgx_passthrough() {
    let mode = TeeMode::GramineDirect;
    assert!(mode.enabled());
    assert!(!mode.uses_mock_binary());
    assert!(!mode.passes_sgx_devices());
    assert!(mode.uses_deterministic_dkg_seed());
    assert_eq!(mode.evidence_name(), "gramine-direct");
    assert!(mode.satisfies_gramine_direct_requirement());
    assert!(!TeeMode::Mock.satisfies_gramine_direct_requirement());
    assert!(!TeeMode::Real.satisfies_gramine_direct_requirement());
}

/// The native profile runs the same mock binary and the same deterministic
/// seed, but outside Gramine. Therefore it must carry its own evidence label and
/// must not stand in for any profile that proves a Gramine or SGX property.
#[test]
fn native_host_mode_is_a_distinct_profile_that_proves_no_gramine_property() {
    let mode = TeeMode::MockNative;
    assert!(mode.enabled());
    assert!(mode.uses_mock_binary());
    assert!(mode.runs_native_host_enclave());
    assert!(mode.uses_deterministic_dkg_seed());
    assert!(!mode.passes_sgx_devices());
    assert_eq!(mode.evidence_name(), "mock-native");
    assert!(!mode.satisfies_gramine_direct_requirement());
    assert!(!mode.satisfies_sgx_no_attest_requirement());

    // Every containerized profile keeps running under Gramine.
    for containerized in [
        TeeMode::Real,
        TeeMode::SgxNoAttest,
        TeeMode::GramineDirect,
        TeeMode::Mock,
    ] {
        assert!(
            !containerized.runs_native_host_enclave(),
            "{containerized:?} must stay containerized"
        );
        assert_ne!(containerized.evidence_name(), mode.evidence_name());
    }
}

/// Both mock profiles select the mock binary. Only the wrapper differs.
#[test]
fn native_host_mode_selects_the_mock_enclave_binary() {
    let env = Environment {
        tee_mode: TeeMode::MockNative,
        mock_bin: PathBuf::from("/artifact-set/outbe-tee-enclave-mock"),
        enclave_bin: PathBuf::from("/artifact-set/outbe-tee-enclave"),
        ..Environment::default()
    };
    assert_eq!(
        env.selected_enclave_bin(),
        Path::new("/artifact-set/outbe-tee-enclave-mock")
    );
}

#[test]
fn gramine_direct_selects_the_exact_production_enclave_binary() {
    let env = Environment {
        tee_mode: TeeMode::GramineDirect,
        enclave_bin: PathBuf::from("/artifact-set/outbe-tee-enclave"),
        mock_bin: PathBuf::from("/artifact-set/outbe-tee-enclave-mock"),
        ..Environment::default()
    };
    assert_eq!(
        env.selected_enclave_bin(),
        Path::new("/artifact-set/outbe-tee-enclave")
    );
}

#[cfg(unix)]
#[test]
fn default_data_dir_keeps_unix_socket_paths_short() {
    let path = default_data_dir()
        .join("run-1785161738-87200")
        .join("scenario-1/validator-0/data/reth.ipc");

    assert!(path.as_os_str().len() < 104, "{}", path.display());
}

#[test]
fn sgx_no_attest_uses_production_enclave_and_real_sgx_without_dev_seed() {
    let mode = TeeMode::SgxNoAttest;
    assert!(mode.enabled());
    assert!(!mode.uses_mock_binary());
    assert!(mode.passes_sgx_devices());
    assert!(!mode.uses_deterministic_dkg_seed());
    assert_eq!(mode.evidence_name(), "sgx-no-attest");
    assert!(mode.satisfies_sgx_no_attest_requirement());
    assert!(!TeeMode::Real.satisfies_sgx_no_attest_requirement());
    assert!(!TeeMode::GramineDirect.satisfies_sgx_no_attest_requirement());

    let env = Environment {
        tee_mode: mode,
        enclave_bin: PathBuf::from("/artifact-set/outbe-tee-enclave"),
        mock_bin: PathBuf::from("/artifact-set/outbe-tee-enclave-mock"),
        ..Environment::default()
    };
    assert_eq!(
        env.selected_enclave_bin(),
        Path::new("/artifact-set/outbe-tee-enclave")
    );
}

#[test]
fn explicit_sudo_overrides_docker_reachability() {
    assert!(resolve_sudo(true, false));
    assert!(!resolve_sudo(false, true));
}

#[test]
fn parses_min_validators_tag() {
    assert_eq!(parse_min_validators_tag("min-validators-4"), Some(4));
    assert_eq!(parse_min_validators_tag("min-validators-12"), Some(12));
    assert_eq!(parse_min_validators_tag("tee"), None);
    assert_eq!(parse_min_validators_tag("min-validators-"), None);
    assert_eq!(parse_min_validators_tag("min-validators-x"), None);
}

#[test]
fn parses_exact_validators_tag() {
    assert_eq!(parse_exact_validators_tag("validators-4"), Some(4));
    assert_eq!(parse_exact_validators_tag("validators-12"), Some(12));
    assert_eq!(parse_exact_validators_tag("min-validators-4"), None);
    assert_eq!(parse_exact_validators_tag("validators-"), None);
    assert_eq!(parse_exact_validators_tag("validators-x"), None);
}

#[test]
fn exact_validator_requirement_rejects_a_larger_committee() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/price_oracle.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .expect("parse price Oracle feature");
    let scenario = feature.scenarios.first().expect("price Oracle scenario");
    let mut env = Environment {
        validators: 4,
        ..Environment::default()
    };
    assert_eq!(unmet(&feature, scenario, &env), None);

    env.validators = 5;
    assert_eq!(
        unmet(&feature, scenario, &env).as_deref(),
        Some("needs exactly 4 validators, have 5")
    );
}

#[test]
fn real_sgx_requirement_stays_skipped_under_all() {
    let reason = "needs DcapRequired".to_string();
    assert_eq!(
        decide_requirement(Some(reason.clone()), true, true),
        Decision::Skip(reason)
    );
    assert_eq!(
        decide_requirement(Some("ordinary requirement".to_string()), true, false),
        Decision::Run
    );
}

#[test]
fn parsed_real_sgx_scenario_is_skipped_by_sgx_no_attest_all_lane() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/tee_onboarding.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .expect("parse TEE onboarding feature");
    let scenario = feature.scenarios.first().expect("onboarding scenario");
    let env = Environment {
        tee_mode: TeeMode::SgxNoAttest,
        all: true,
        ..Environment::default()
    };

    assert!(matches!(
        decide(&feature, scenario, &env),
        Decision::Skip(_)
    ));
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn inbox_tribute_to_coen_scenario_is_runnable_with_registered_steps() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/ocomp.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .expect("parse Tribute scenarios");
    let scenario = feature
        .scenarios
        .iter()
        .find(|scenario| has_tag(&feature, scenario, "tribute-inbox-key"))
        .expect("inbox-backed real proof scenario");
    let env = Environment {
        tee_mode: TeeMode::SgxNoAttest,
        validators: 4,
        sudo: true,
        all: true,
        ..Environment::default()
    };
    assert_eq!(unmet(&feature, scenario, &env), None);
    assert_eq!(decide(&feature, scenario, &env), Decision::Run);
    assert!(has_tag(&feature, scenario, "ocomp-public-apply"));
    let step_index = |text: &str| {
        scenario
            .steps
            .iter()
            .position(|step| step.value == text)
            .unwrap_or_else(|| panic!("main flow is missing step: {text}"))
    };
    let registration = step_index(
        "an L2 network is registered through governance with an inbox contract and no pinned key",
    );
    let offer = step_index("a user of the registered L2 submits an encrypted Tribute with ZKP and WAA and SRA beneficiaries");
    let nod =
        step_index("three matching validator domains atomically apply Lysis and create the Nod");
    let coen = step_index(
        "the public Tribute owner settles its Nod and redeems its exact Gratis into COEN",
    );
    assert!(registration < offer && offer < nod && nod < coen);
    assert_registered_steps(&feature, scenario);
}

#[test]
fn explicit_tee_profiles_are_disjoint_even_under_all() {
    let no_attest_feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/tribute.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .expect("parse SGX-no-attest feature");
    let no_attest = no_attest_feature
        .scenarios
        .first()
        .expect("SGX-no-attest scenario");
    let mut direct_feature = no_attest_feature.clone();
    direct_feature.tags.retain(|tag| tag != "sgx-no-attest");
    direct_feature.tags.push("gramine-direct".to_owned());
    let direct = direct_feature
        .scenarios
        .first()
        .expect("gramine-direct scenario");

    let no_attest_env = Environment {
        tee_mode: TeeMode::SgxNoAttest,
        all: true,
        sudo: true,
        ..Environment::default()
    };
    assert_eq!(
        decide(&no_attest_feature, no_attest, &no_attest_env),
        Decision::Run
    );
    let intex = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/intex.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .expect("parse Intex SGX feature");
    assert_eq!(intex.scenarios.len(), 2);
    for scenario in &intex.scenarios {
        assert_eq!(decide(&intex, scenario, &no_attest_env), Decision::Run);
        if cfg!(feature = "ocomp-integration") {
            assert_registered_steps(&intex, scenario);
        } else {
            // --all selects the scenario, but the before hook must report
            // that its @ocomp capability is absent from this build.
            assert!(unmet(&intex, scenario, &no_attest_env)
                .expect("missing OCOMP build capability")
                .contains("built without --features ocomp-integration"));
        }
    }
    assert!(matches!(
        decide(&direct_feature, direct, &no_attest_env),
        Decision::Skip(_)
    ));

    let direct_env = Environment {
        tee_mode: TeeMode::GramineDirect,
        all: true,
        ..Environment::default()
    };
    assert_eq!(decide(&direct_feature, direct, &direct_env), Decision::Run);
    assert!(matches!(
        decide(&no_attest_feature, no_attest, &direct_env),
        Decision::Skip(_)
    ));
}
