use super::*;
#[cfg(feature = "ocomp-integration")]
use std::path::PathBuf;

use crate::verification_ledger::{SourceIdentityV1, TestDiscoveryV1};

#[test]
fn hand_authored_pass_without_runner_receipt_is_rejected() {
    let evidence = DomainEvidenceV1 {
        schema_version: 1,
        namespace: METADOSIS_NAMESPACE.to_owned(),
        source: SourceIdentityV1 {
            sha: "a".repeat(40),
            tracked_dirty: false,
            untracked_dirty: false,
            cargo_lock_sha256: "b".repeat(64),
            rust_toolchain_sha256: "c".repeat(64),
            dependency_metadata_sha256: "d".repeat(64),
        },
        artifact_profile: "OUTBE_OCOMP_POC_DEVNET_FORK_V1".to_owned(),
        lane: "METADOSIS-LINUX".to_owned(),
        linux_image_digest: format!("sha256:{}", "e".repeat(64)),
        discovery: TestDiscoveryV1 {
            discovered: vec!["MET-FAKE-001".to_owned()],
            ..TestDiscoveryV1::default()
        },
        tests: vec![EvidenceTestV1 {
            test_id: "MET-FAKE-001".to_owned(),
            target: "fake".to_owned(),
            name: "fake".to_owned(),
            lane: "METADOSIS-LINUX".to_owned(),
            production_interface: "fake".to_owned(),
            substitutions: Vec::new(),
            status: AssertionStatus::Pass,
            evidence_member_refs: vec!["fake.log".to_owned()],
        }],
        members: vec![MemberDigestV1 {
            path: "fake.log".to_owned(),
            length: 0,
            sha256: "f".repeat(64),
        }],
    };
    let error = require_runner_derivation_member(&evidence).unwrap_err();
    assert!(error.to_string().contains("runner-receipt.json"));
}

#[test]
fn status_is_derived_only_from_discovery_ignore_and_exit_facts() {
    assert_eq!(
        derive_status(true, false, true, false),
        AssertionStatus::Pass
    );
    assert_eq!(
        derive_status(true, false, false, false),
        AssertionStatus::Fail
    );
    assert_eq!(
        derive_status(true, false, false, true),
        AssertionStatus::InfraError
    );
    assert_eq!(
        derive_status(false, false, true, false),
        AssertionStatus::Missing
    );
    assert_eq!(
        derive_status(true, true, true, false),
        AssertionStatus::Skipped
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn fresh_devnet_process_verifier_requires_measurement_one_layout_and_terminal_path() {
    let root = tempfile::tempdir().unwrap();
    let fresh = root.path().join("fresh-devnet");
    std::fs::create_dir_all(fresh.join("evidence")).unwrap();
    let artifacts = fresh.join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    std::fs::write(fresh.join("commands.log"), b"exact fresh command\n").unwrap();
    std::fs::write(fresh.join("run.log"), b"scenario passed\n").unwrap();
    let source = SourceIdentityV1 {
        sha: "a".repeat(40),
        tracked_dirty: false,
        untracked_dirty: false,
        cargo_lock_sha256: "b".repeat(64),
        rust_toolchain_sha256: "c".repeat(64),
        dependency_metadata_sha256: "d".repeat(64),
    };
    let runner = MetadosisRunReceiptV1 {
        schema_version: METADOSIS_RUN_RECEIPT_SCHEMA_VERSION,
        kind: METADOSIS_RUN_RECEIPT_KIND.to_owned(),
        namespace: METADOSIS_NAMESPACE.to_owned(),
        source: source.clone(),
        artifact_profile: "OUTBE_OCOMP_POC_DEVNET_FORK_V1".to_owned(),
        lane: "METADOSIS-LINUX".to_owned(),
        linux_image_digest: format!("sha256:{}", "e".repeat(64)),
        launcher_nonce: "f".repeat(64),
        tests: Vec::new(),
    };
    std::fs::write(
        root.path().join(RUNNER_RECEIPT_MEMBER),
        serde_json::to_vec(&runner).unwrap(),
    )
    .unwrap();

    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut genesis: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            repo.join("testing/e2e-harness/fixtures/ocomp-final-v1/artifacts/genesis-final.json"),
        )
        .unwrap(),
    )
    .unwrap();
    genesis["config"]["outbeProtocol"] = serde_json::json!({
        "schemaVersion": 1,
        "metadosis": {
            "formingPeriodSeconds": 57600,
            "lookbackDelaySeconds": 0,
            "offeringPeriodSeconds": 900,
            "waitingPeriodSeconds": 30,
            "advanceIntervalSeconds": 10
        },
        "ocomp": { "computeVoteWindowBlocks": 120 }
    });
    let genesis_path = artifacts.join("genesis.json");
    std::fs::write(&genesis_path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();
    let base_spec =
        reth_ethereum::cli::chainspec::chain_value_parser(genesis_path.to_str().unwrap())
            .unwrap()
            .as_ref()
            .clone()
            .map_header(OutbeHeader::new);
    let chain_id = base_spec.chain().id();
    let genesis_hash_value = base_spec.genesis_hash();
    let limits = poc_schema_limits();
    let install = ForkInstallScenario::measurement_at(1, chain_id, genesis_hash_value)
        .unwrap()
        .into_install();
    install
        .validate_for_chain(chain_id, genesis_hash_value, &limits)
        .unwrap();
    let canonical_install = install.encode_canonical(&limits).unwrap();
    let install_hash = install.install_hash(&limits).unwrap();
    let protocol_bundle_hash = install
        .protocol_bundle
        .protocol_bundle_hash(&limits)
        .unwrap();
    let config = genesis["config"].as_object_mut().unwrap();
    config.insert(
        "ocompForkInstallV1".to_owned(),
        serde_json::json!({
            "canonicalBytes": format!("0x{}", hex::encode(&canonical_install)),
            "installHash": install_hash,
        }),
    );
    config.insert(
        "metadosisStorageLayoutV1".to_owned(),
        serde_json::json!({
            "layoutHash": crate::world::ocomp::METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX,
        }),
    );
    std::fs::write(&genesis_path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();
    std::fs::write(artifacts.join("fork-install-v1.ocb1"), &canonical_install).unwrap();
    std::fs::write(
        artifacts.join("protocol-bundle-v1.ocb1"),
        install.protocol_bundle.encode_canonical(&limits).unwrap(),
    )
    .unwrap();
    std::fs::write(
        artifacts.join("capacity-profile-v1.ocb1"),
        install
            .request_profile
            .capacity_profile
            .encode_canonical(&limits)
            .unwrap(),
    )
    .unwrap();
    let mut exact_binaries = serde_json::Map::new();
    for (identity, file_name) in [
        ("outbe_chain", "outbe-chain"),
        ("outbe_ocomp", "outbe-ocomp"),
        ("outbe_feeder", "outbe-feeder"),
        ("outbe_cli", "outbe-cli"),
        ("outbe_keygen", "outbe-keygen"),
        ("outbe_e2e", "outbe-e2e"),
        ("outbe_tee_enclave", "outbe-tee-enclave"),
    ] {
        let binary = artifacts.join(file_name);
        std::fs::write(&binary, format!("exact {file_name} bytes")).unwrap();
        exact_binaries.insert(
            identity.to_owned(),
            serde_json::to_value(crate::ocomp_evidence::hash_file(&binary).unwrap()).unwrap(),
        );
    }
    let mut processes = Vec::new();
    for validator_index in 0..4 {
        for role in ["snapshot_exporter", "worker"] {
            processes.push(serde_json::json!({
                "validator_index": validator_index,
                "role": role,
                "stopped_at_millis": null
            }));
        }
    }
    let finalized_points = |block_number: u64, block_timestamp: u64, hash_byte: u8| {
        (0..4)
            .map(|validator_index| {
                serde_json::json!({
                    "validator_index": validator_index,
                    "block_number": block_number,
                    "block_hash": format!("0x{}", format!("{hash_byte:02x}").repeat(32)),
                    "block_timestamp": block_timestamp
                })
            })
            .collect::<Vec<_>>()
    };
    let forming_start = 1_785_319_200_u64;
    let initial_timestamp = forming_start + 15 * 3_600;
    let forming_end = forming_start + 16 * 3_600;
    let offering_end = forming_end + 900;
    let scheduled_process_time = offering_end + 30;
    let first_target = forming_end + 1;
    let second_target = scheduled_process_time + 1;
    let before_first_restart = finalized_points(1, initial_timestamp, 0x44);
    let after_first_restart = finalized_points(2, first_target, 0x55);
    let before_second_restart = finalized_points(2, first_target, 0x55);
    let after_second_restart = finalized_points(3, second_target, 0x66);
    let worldwide_day = 20_260_730_u64;
    let genesis_hash = format!("{genesis_hash_value:#x}");
    let scenario_path = fresh.join("evidence/scenario-001.json");
    let oracle_base = format!("{:#x}", alloy_primitives::Address::ZERO);
    let oracle_quote = format!("{:#x}", outbe_primitives::asset_type::currency_address(840));
    let oracle_processes = (1..=3_u32)
        .flat_map(|attempt| (0..3).map(move |validator_index| {
            crate::world::price_oracle::FeederProcessEvidenceV1 {
                validator_index,
                validator_address: format!("0x{:040x}", validator_index + 1),
                rpc_endpoint: format!("http://127.0.0.1:{}", 18_545 + validator_index),
                vote_period: 8,
                attempt,
                pid: 12_345 + 100 * attempt + u32::try_from(validator_index).unwrap(),
                log: format!(
                    "fresh-devnet/evidence/price-oracle/validator-{validator_index}-feeder-attempt-{attempt}.log"
                ),
                started_at_millis: u64::from(attempt) * 100,
                stopped_at_millis: (attempt < 3).then_some(u64::from(attempt) * 100 + 10),
                oracle_pairs: vec!["COEN/840".to_owned()],
                source_markets: vec!["mock_http:COEN/840".to_owned()],
            }
        }))
        .collect::<Vec<_>>();
    let mut oracle_sources = (0..3)
        .flat_map(|validator_index| {
            [
                crate::world::price_oracle::ControlledSourceEvidenceV1 {
                    phase: crate::world::price_oracle::OracleEvidencePhaseV1::Initial,
                    generation: 1,
                    validator_index,
                    oracle_pair: "COEN/840".to_owned(),
                    source_market: "mock_http:COEN/840".to_owned(),
                    price: "1.000000".to_owned(),
                    volume: "1000.000000".to_owned(),
                },
                crate::world::price_oracle::ControlledSourceEvidenceV1 {
                    phase: crate::world::price_oracle::OracleEvidencePhaseV1::ControlledUpdate,
                    generation: 2,
                    validator_index,
                    oracle_pair: "COEN/840".to_owned(),
                    source_market: "mock_http:COEN/840".to_owned(),
                    price: "1.080001".to_owned(),
                    volume: "1000.000000".to_owned(),
                },
            ]
        })
        .collect::<Vec<_>>();
    oracle_sources.extend((0..3).flat_map(|validator_index| {
        (0..2).map(
            move |_| crate::world::price_oracle::ControlledSourceEvidenceV1 {
                phase: crate::world::price_oracle::OracleEvidencePhaseV1::ClockRestart,
                generation: 1,
                validator_index,
                oracle_pair: "COEN/840".to_owned(),
                source_market: "mock_http:COEN/840".to_owned(),
                price: "1.000000".to_owned(),
                volume: "1000.000000".to_owned(),
            },
        )
    }));
    let oracle_publication = |phase, rate: &str, block, timestamp| {
        crate::world::price_oracle::CanonicalPricePublicationV1 {
            phase,
            validator_count: 4,
            base: oracle_base.clone(),
            quote: oracle_quote.clone(),
            rate: rate.to_owned(),
            volume: "1000000000".to_owned(),
            oracle_block: block,
            oracle_timestamp: timestamp,
            finalized_height: block,
            finalized_timestamp: timestamp,
            age_seconds: 0,
        }
    };
    let mut price_oracle = crate::world::price_oracle::PriceOracleEvidenceV1 {
        feeder_binary: artifacts.join("outbe-feeder").display().to_string(),
        feeder_binary_sha256: exact_binaries["outbe_feeder"]["sha256"]
            .as_str()
            .unwrap()
            .to_owned(),
        feeder_processes: oracle_processes,
        ticker_requests: 3,
        candle_requests: 3,
        controlled_sources: oracle_sources,
        canonical_publications: vec![
            oracle_publication(
                crate::world::price_oracle::OracleEvidencePhaseV1::Initial,
                "1000000",
                2,
                initial_timestamp,
            ),
            oracle_publication(
                crate::world::price_oracle::OracleEvidencePhaseV1::ClockRestart,
                "1000000",
                4,
                first_target,
            ),
            oracle_publication(
                crate::world::price_oracle::OracleEvidencePhaseV1::ClockRestart,
                "1000000",
                6,
                second_target,
            ),
            oracle_publication(
                crate::world::price_oracle::OracleEvidencePhaseV1::ControlledUpdate,
                "1080001",
                8,
                second_target + 2,
            ),
        ],
        quorum_loss_windows: Vec::new(),
        penalty_snapshots: Vec::new(),
        cohorts: Vec::new(),
    };
    // The fixture models the initial launch and both real feeder restarts.
    // Every publication carries the same mandatory cohort proof as a run.
    for (attempt, publication_indexes) in [(1, vec![0]), (2, vec![1]), (3, vec![2, 3])] {
        use crate::world::price_oracle::{
            OracleCheckpointV1, OracleCohortEvidenceV1, OracleCohortV1, OracleMemberV1,
        };
        let first = &price_oracle.canonical_publications[publication_indexes[0]];
        let members = (0..4)
            .map(|index| OracleMemberV1 {
                index,
                address: format!("0x{:040x}", index + 1).parse().unwrap(),
                port: 18_545 + u16::try_from(index).unwrap(),
                node_pid: 10_000 + 100 * attempt + u32::try_from(index).unwrap(),
            })
            .collect::<Vec<_>>();
        let attempts = price_oracle
            .feeder_processes
            .iter()
            .filter(|process| process.attempt == attempt)
            .map(|process| (process.validator_index, process.attempt, process.pid))
            .collect::<Vec<_>>();
        let cohort = OracleCohortV1 {
            checkpoint: OracleCheckpointV1 {
                height: first.oracle_block - 1,
                block_hash: alloy_primitives::B256::repeat_byte(0x44),
                state_root: alloy_primitives::B256::repeat_byte(0x45),
            },
            members,
            quorum: 3,
            feeder_indices: vec![0, 1, 2],
            overlapping_pairs: false,
        };
        let observations =
            publication_indexes
                .into_iter()
                .map(|index| {
                    let publication = &price_oracle.canonical_publications[index];
                    let checkpoint = OracleCheckpointV1 {
                        height: publication.finalized_height,
                        block_hash: alloy_primitives::B256::repeat_byte(0x55),
                        state_root: alloy_primitives::B256::repeat_byte(0x56),
                    };
                    let reads = cohort.members.iter().map(|member| serde_json::json!({
                "port": member.port, "finalized": checkpoint.height,
                "checkpoint": checkpoint,
                "rate": publication.rate.parse::<alloy_primitives::U256>().unwrap(),
                "volume": publication.volume.parse::<alloy_primitives::U256>().unwrap(),
                "oracle_block": publication.oracle_block,
                "oracle_timestamp": publication.oracle_timestamp,
                "finalized_timestamp": publication.finalized_timestamp,
            })).collect::<Vec<_>>();
                    serde_json::json!({
                        "process_history_len": attempt * 3,
                        "current_attempts": attempts,
                        "observation": {
                            "kind": "publication_verified", "phase": publication.phase,
                            "base": publication.base, "quote": publication.quote,
                            "oracle_block": publication.oracle_block,
                            "strictly_after_block": publication.oracle_block - 1,
                            "checkpoint": checkpoint, "reads": reads,
                        },
                    })
                })
                .collect();
        price_oracle.cohorts.push(OracleCohortEvidenceV1 {
            first_process_record: usize::try_from((attempt - 1) * 3).unwrap(),
            phase: first.phase,
            cohort,
            current_attempts: if attempt == 3 { attempts } else { Vec::new() },
            observations,
        });
    }
    let mut scenario = serde_json::json!({
        "source": {
            "sha": source.sha,
            "tracked_dirty": false,
            "untracked_dirty": false
        },
        "feature": "Off-chain computation and Metadosis",
        "scenario": "A public Tribute completes real OCOMP, FullNode verification, NOD, and replay",
        "result": "passed",
        "environment": {
            "validators": 4,
            "tee": "gramine-direct",
            "all": true
        },
        "price_oracle": price_oracle,
        "ocomp": {
            "exact_binaries": exact_binaries,
            "topology": {
                "launch_identity": {
                    "classification": "measurement",
                    "activation_height": 1,
                    "chain_id": chain_id,
                    "genesis_hash": genesis_hash,
                    "metadosis_storage_layout_hash":
                        crate::world::ocomp::METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX,
                    "fork_install_hash": format!("{install_hash:#x}"),
                    "protocol_bundle_hash": format!("{protocol_bundle_hash:#x}")
                },
                "processes": processes
            },
            "public_path": {
                "atomic_quorum_apply_verified": true,
                "job_request": {
                    "worldwide_day": worldwide_day
                },
                "activation": {
                    "worldwide_day": worldwide_day,
                    "terminal_receipt_hash": format!("0x{}", "77".repeat(32))
                },
                "certified_generation": {
                    "worldwide_day": worldwide_day
                },
                "capacity_public_path": {
                    "q_forming_receipt_success": true,
                    "canonical_import_verified": true,
                    "canonical_import_validator_count": 4,
                    "tribute_count": 257,
                    "nod_count": 257
                },
                "metadosis_fresh_lifecycle": {
                    "worldwide_day": worldwide_day,
                    "genesis_hash": genesis_hash,
                    "initial_timestamp": initial_timestamp,
                    "initial_unix_time_offset_secs": -10,
                    "forming_start": forming_start,
                    "forming_end": forming_end,
                    "lookback_end": forming_end,
                    "offering_end": offering_end,
                    "scheduled_process_time": scheduled_process_time,
                    "started": {
                        "worldwide_day": worldwide_day,
                        "forming_start": forming_start,
                        "forming_end": forming_end,
                        "lookback_end": forming_end,
                        "offering_end": offering_end,
                        "scheduled_process_time": scheduled_process_time,
                        "block_number": 1,
                        "block_hash": format!("0x{}", "44".repeat(32))
                    },
                    "status_changes": [
                        {
                            "worldwide_day": worldwide_day,
                            "old_status": 0,
                            "new_status": 1,
                            "block_number": 2,
                            "block_hash": format!("0x{}", "55".repeat(32))
                        },
                        {
                            "worldwide_day": worldwide_day,
                            "old_status": 1,
                            "new_status": 2,
                            "block_number": 2,
                            "block_hash": format!("0x{}", "55".repeat(32))
                        },
                        {
                            "worldwide_day": worldwide_day,
                            "old_status": 2,
                            "new_status": 3,
                            "block_number": 3,
                            "block_hash": format!("0x{}", "66".repeat(32))
                        },
                        {
                            "worldwide_day": worldwide_day,
                            "old_status": 3,
                            "new_status": 4,
                            "block_number": 3,
                            "block_hash": format!("0x{}", "66".repeat(32))
                        }
                    ],
                    "time_control_epochs": [
                        {
                            "requested_timestamp": first_target,
                            "unix_time_offset_secs": 10,
                            "before_restart": before_first_restart,
                            "after_restart": after_first_restart
                        },
                        {
                            "requested_timestamp": second_target,
                            "unix_time_offset_secs": 20,
                            "before_restart": before_second_restart,
                            "after_restart": after_second_restart
                        }
                    ],
                    "created_validator_count": 4,
                    "unknown_status_revert_validator_count": 4,
                    "offering_validator_count": 4,
                    "ready_validator_count": 4,
                    "completed_validator_count": 4
                }
            }
        }
    });
    std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
    let dummy_member = MemberDigestV1 {
        path: "unused".to_owned(),
        length: 1,
        sha256: "f".repeat(64),
    };
    let receipt = crate::metadosis_evidence::schema::TestRunReceiptV1 {
        test_id: METADOSIS_FRESH_DEVNET_TEST_ID.to_owned(),
        discovery: None,
        execution: CommandReceiptV1 {
            argv: vec![
                "outbe-metadosis-evidence".to_owned(),
                "fresh-devnet".to_owned(),
                "--output".to_owned(),
                root.path().join("fresh-devnet").display().to_string(),
            ],
            attempt: 1,
            started_at_unix_ms: 1,
            finished_at_unix_ms: 2,
            exit_code: Some(0),
            stdout: dummy_member.clone(),
            stderr: dummy_member,
        },
        process_artifacts: Vec::new(),
    };
    verify_fresh_devnet_process(root.path(), &receipt).unwrap();

    // Verify capture ordering through the complete verifier, not only the
    // publication helper: a proof cannot borrow another launch's history.
    for defect in 0..4 {
        let mut wrong = scenario.clone();
        let cohorts = &mut wrong["price_oracle"]["cohorts"];
        match defect {
            0 => cohorts[0]["observations"][0]["process_history_len"] = serde_json::json!(4),
            1 => {
                cohorts[2]["observations"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({
                        "process_history_len": 8,
                        "current_attempts": [],
                        "observation": {"kind": "diagnostic"},
                    }));
            }
            2 => {
                cohorts[1]
                    .as_object_mut()
                    .unwrap()
                    .remove("first_process_record");
            }
            _ => cohorts[1]["first_process_record"] = serde_json::json!(0),
        }
        assert!(
            verify_price_oracle_evidence(&wrong).is_err(),
            "accepted history defect {defect}"
        );
    }

    scenario["ocomp"]["topology"]["processes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "validator_index": 0,
            "role": "supervisor",
            "stopped_at_millis": null
        }));
    std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
    assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());
    scenario["ocomp"]["topology"]["processes"]
        .as_array_mut()
        .unwrap()
        .pop();
    std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();

    let retained_chain = std::fs::read(artifacts.join("outbe-chain")).unwrap();
    std::fs::write(artifacts.join("outbe-chain"), b"tampered binary").unwrap();
    assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());
    std::fs::write(artifacts.join("outbe-chain"), retained_chain).unwrap();

    let retained_bundle = std::fs::read(artifacts.join("protocol-bundle-v1.ocb1")).unwrap();
    std::fs::remove_file(artifacts.join("protocol-bundle-v1.ocb1")).unwrap();
    assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());
    std::fs::write(artifacts.join("protocol-bundle-v1.ocb1"), retained_bundle).unwrap();

    scenario["ocomp"]["public_path"]["metadosis_fresh_lifecycle"]
        ["unknown_status_revert_validator_count"] = serde_json::json!(3);
    std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
    assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());

    scenario["ocomp"]["public_path"]["metadosis_fresh_lifecycle"]
        ["unknown_status_revert_validator_count"] = serde_json::json!(4);
    scenario["price_oracle"]["canonical_publications"][1]["age_seconds"] =
        serde_json::json!(21_601);
    std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
    assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());

    scenario["price_oracle"]["canonical_publications"][1]["age_seconds"] = serde_json::json!(0);
    let removed_feeder = scenario["price_oracle"]["feeder_processes"]
        .as_array_mut()
        .unwrap()
        .pop()
        .unwrap();
    std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
    assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());
    scenario["price_oracle"]["feeder_processes"]
        .as_array_mut()
        .unwrap()
        .push(removed_feeder);

    scenario["price_oracle"]["canonical_publications"][3]["rate"] = serde_json::json!("1");
    std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
    assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());
    scenario["price_oracle"]["canonical_publications"][3]["rate"] = serde_json::json!("1080001");

    scenario["price_oracle"]["controlled_sources"][5]["validator_index"] = serde_json::json!(1);
    std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
    assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());
    scenario["price_oracle"]["controlled_sources"][5]["validator_index"] = serde_json::json!(2);

    scenario["ocomp"]["public_path"]["metadosis_fresh_lifecycle"]["status_changes"][0]
        ["new_status"] = serde_json::json!(2);
    std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
    assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());
}
