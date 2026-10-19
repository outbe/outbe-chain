use super::*;
use std::path::Path;

#[test]
fn state_persistence_scenario_is_registered_for_sgx_lanes() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/state_persistence.feature"),
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
fn flattened_env_cli_parses_with_default_validator_count() {
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        env: EnvCli,
    }
    assert_eq!(Cli::try_parse_from(["e2e"]).unwrap().env.validators, 4);
}

#[test]
fn native_state_persistence_scenario_runs_only_in_its_explicit_profile_even_with_all() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/state_persistence_native.feature"),
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
                assert_registered_steps(&feature, scenario);
                checked.push(scenario.name.clone());
            }
        }
    }
    assert_eq!(
        checked,
        [
            "Per-pair quorum survives a sub-quorum cross intersection",
            "Feeder source failures leave the last finalized price intact and recover",
            "Exhausted quota resets lazily across the worldwide-day boundary",
        ]
    );
}

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
    for file in ["settlement.feature", "lysis.feature"] {
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
    assert_eq!(
        checked,
        [
            "A zero-balance validator redeems its reward Gem through ZeroFee",
            "A stale Oracle rate defers validator Gem delivery and later recovers",
            "A public Tribute becomes an encrypted Nod and its owner receives COEN",
        ]
    );
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

#[test]
fn inbox_tribute_to_coen_scenario_is_runnable_with_registered_steps() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/lysis.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .expect("parse Tribute scenarios");
    let scenario = feature
        .scenarios
        .iter()
        .find(|scenario| has_tag(&feature, scenario, "nod-settlement"))
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
    let nod = step_index(
        "Metadosis completes confidential Lysis and stores ready Nods on every validator",
    );
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
        assert_registered_steps(&intex, scenario);
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

#[test]
fn consensus_resilience_scenarios_have_registered_steps() {
    let feature = Feature::parse_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("features/consensus_resilience.feature"),
        cucumber::gherkin::GherkinEnv::default(),
    )
    .expect("parse consensus_resilience feature");
    assert_eq!(feature.scenarios.len(), 9);
    for scenario in &feature.scenarios {
        assert_registered_steps(&feature, scenario);
    }
}

#[test]
fn every_feature_step_resolves_to_one_definition() {
    use cucumber::World as _;

    let steps = crate::world::World::collection();
    let unresolved = parse_features("features")
        .iter()
        .flat_map(|feature| unresolved_steps(&steps, feature, |_| false))
        .collect::<Vec<_>>();
    assert!(unresolved.is_empty(), "{}", unresolved.join("\n"));
}

#[test]
fn every_release_feature_step_resolves_to_one_definition() {
    use cucumber::World as _;

    let steps = crate::release_sgx::ReleaseSgxWorld::collection();
    let unresolved = parse_features("release-features")
        .iter()
        .flat_map(|feature| unresolved_steps(&steps, feature, |_| false))
        .collect::<Vec<_>>();
    assert!(unresolved.is_empty(), "{}", unresolved.join("\n"));
}

fn parse_features(dir: &str) -> Vec<Feature> {
    let mut paths = walkdir::WalkDir::new(Path::new(env!("CARGO_MANIFEST_DIR")).join(dir))
        .into_iter()
        .map(|entry| entry.expect("walk feature directory").into_path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "feature"))
        .collect::<Vec<_>>();
    paths.sort();
    assert!(!paths.is_empty(), "no feature files under {dir}");
    paths
        .iter()
        .map(|path| {
            Feature::parse_path(path, cucumber::gherkin::GherkinEnv::default())
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
        })
        .collect()
}

fn unresolved_steps<W>(
    steps: &cucumber::step::Collection<W>,
    feature: &Feature,
    skip: impl Fn(&Scenario) -> bool,
) -> Vec<String> {
    let feature_background = feature.background.iter().flat_map(|b| &b.steps);
    let top_level = feature
        .scenarios
        .iter()
        .map(|scenario| (feature_background.clone().collect::<Vec<_>>(), scenario));
    let in_rules = feature.rules.iter().flat_map(|rule| {
        let background = feature_background
            .clone()
            .chain(rule.background.iter().flat_map(|b| &b.steps))
            .collect::<Vec<_>>();
        rule.scenarios
            .iter()
            .map(move |scenario| (background.clone(), scenario))
    });
    top_level
        .chain(in_rules)
        .filter(|(_, scenario)| !skip(scenario))
        .flat_map(|(background, scenario)| {
            background
                .into_iter()
                .chain(&scenario.steps)
                .filter_map(|step| match steps.find(step) {
                    Ok(Some(_)) => None,
                    Ok(None) => Some(format!(
                        "{}: unregistered {:?} step: {}",
                        scenario.name, step.ty, step.value
                    )),
                    Err(error) => Some(format!("{}: {error}", scenario.name)),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}
