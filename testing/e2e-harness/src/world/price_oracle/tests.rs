use super::*;

fn cohort_fixture(overlapping_pairs: bool) -> OracleCohortV1 {
    OracleCohortV1 {
        checkpoint: OracleCheckpointV1 {
            height: 20,
            block_hash: alloy_primitives::B256::repeat_byte(1),
            state_root: alloy_primitives::B256::repeat_byte(2),
        },
        members: (0..4)
            .map(|index| OracleMemberV1 {
                index,
                address: alloy_primitives::Address::repeat_byte(u8::try_from(index + 1).unwrap()),
                port: 8000 + u16::try_from(index).unwrap(),
                node_pid: 100 + u32::try_from(index).unwrap(),
            })
            .collect(),
        quorum: 3,
        feeder_indices: if overlapping_pairs {
            vec![0, 1, 2, 3]
        } else {
            vec![0, 1, 2]
        },
        overlapping_pairs,
    }
}

#[test]
fn stopped_feeder_is_not_replenished_by_publication_checks() {
    let cohort = cohort_fixture(false);
    assert!(validate_live_feeder_indices(&cohort, &[0, 1, 2]).is_ok());
    assert!(validate_live_feeder_indices(&cohort, &[0, 1]).is_err());
    assert!(validate_live_feeder_indices(&cohort, &[0, 1, 1]).is_err());
    assert!(validate_live_feeder_indices(&cohort, &[0, 1, 3]).is_err());
    let overlap = cohort_fixture(true);
    assert!(validate_live_feeder_indices(&overlap, &[0, 1, 3]).is_ok());
    assert!(validate_live_feeder_indices(&overlap, &[0, 1, 2, 3]).is_ok());
    assert!(validate_live_feeder_indices(&overlap, &[0, 1, 4]).is_err());
}

#[test]
fn public_cohort_retains_partial_observations_and_exact_attempts() {
    let evidence = OracleCohortEvidenceV1 {
        first_process_record: 3,
        phase: OracleEvidencePhaseV1::ClockRestart,
        cohort: cohort_fixture(false),
        current_attempts: vec![(0, 2, 101), (1, 2, 102)],
        observations: vec![serde_json::json!({"kind": "publication_error", "port": 8002})],
    };
    let encoded = serde_json::to_value(&evidence).unwrap();
    assert_eq!(
        serde_json::from_value::<OracleCohortEvidenceV1>(encoded).unwrap(),
        evidence
    );
    assert_eq!(evidence.current_attempts.len(), 2); // incomplete launch is retained, not fabricated as quorum
}

#[test]
fn evidence_requires_the_mandatory_cohort_field() {
    let mut encoded = serde_json::to_value(PriceOracleEvidenceV1::default()).unwrap();
    assert_eq!(encoded["cohorts"], serde_json::json!([]));
    encoded.as_object_mut().unwrap().remove("cohorts");
    assert!(serde_json::from_value::<PriceOracleEvidenceV1>(encoded).is_err());
}

#[test]
fn publication_proof_requires_cohort_attempts_and_every_pinned_peer() {
    let cohort = cohort_fixture(false);
    let publication = CanonicalPricePublicationV1 {
        phase: OracleEvidencePhaseV1::Initial,
        validator_count: 4,
        base: format!("{:#x}", alloy_primitives::Address::ZERO),
        quote: format!("{:#x}", alloy_primitives::Address::repeat_byte(9)),
        rate: "1000000".into(),
        volume: "1000000000".into(),
        oracle_block: 21,
        oracle_timestamp: 100,
        finalized_height: 22,
        finalized_timestamp: 102,
        age_seconds: 2,
    };
    let checkpoint = OracleCheckpointV1 {
        height: 22,
        ..cohort.checkpoint.clone()
    };
    let processes = cohort
        .members
        .iter()
        .take(3)
        .map(|member| FeederProcessEvidenceV1 {
            validator_index: member.index,
            validator_address: format!("{:#x}", member.address),
            rpc_endpoint: format!("http://127.0.0.1:{}", member.port),
            vote_period: crate::internal::config::E2E_ORACLE_VOTE_PERIOD_BLOCKS,
            attempt: 1,
            pid: 1000 + u32::try_from(member.index).unwrap(),
            log: format!("validator-{}-feeder-attempt-1.log", member.index),
            started_at_millis: 1,
            stopped_at_millis: None,
            oracle_pairs: vec!["COEN/840".into()],
            source_markets: vec!["mock_http:COEN/840".into()],
        })
        .collect::<Vec<_>>();
    let attempts = processes
        .iter()
        .map(|process| (process.validator_index, process.attempt, process.pid))
        .collect::<Vec<_>>();
    let reads = cohort.members.iter().map(|member| serde_json::json!({
            "port": member.port, "finalized": 22, "checkpoint": checkpoint,
            "rate": alloy_primitives::U256::from(1_000_000), "volume": alloy_primitives::U256::from(1_000_000_000u64),
            "oracle_block": 21, "oracle_timestamp": 100, "finalized_timestamp": 102,
        })).collect::<Vec<_>>();
    let proof = serde_json::json!({
        "process_history_len": 3,
        "current_attempts": attempts,
        "observation": {"kind": "publication_verified", "phase": publication.phase,
            "base": publication.base, "quote": publication.quote, "oracle_block": 21,
            "strictly_after_block": 20, "checkpoint": checkpoint, "reads": reads},
    });
    let evidence = PriceOracleEvidenceV1 {
        feeder_processes: processes,
        cohorts: vec![OracleCohortEvidenceV1 {
            first_process_record: 0,
            phase: OracleEvidencePhaseV1::Initial,
            cohort,
            current_attempts: attempts,
            observations: vec![proof.clone()],
        }],
        ..PriceOracleEvidenceV1::default()
    };
    assert!(verify_publication_cohort(&evidence, &publication).is_ok());
    for field in 0..12 {
        let mut bad = evidence.clone();
        match field {
            0 => bad.cohorts.clear(),
            1 => bad.cohorts[0].observations.clear(),
            2 => bad.cohorts[0].observations.push(proof.clone()),
            3 => {
                bad.cohorts[0].observations[0]["observation"]["reads"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            }
            4 => {
                bad.cohorts[0].observations[0]["observation"]["reads"][3]["finalized"] =
                    serde_json::json!(21)
            }
            5 => bad.cohorts[0].observations[0]["current_attempts"] = serde_json::json!([]),
            6 => {
                bad.cohorts[0].observations[0]["observation"]["strictly_after_block"] =
                    serde_json::json!(21)
            }
            7 => {
                bad.cohorts[0].observations[0]["observation"]["reads"][3]["checkpoint"]
                    ["state_root"] = serde_json::json!(alloy_primitives::B256::ZERO)
            }
            8 => {
                bad.cohorts[0].observations[0]["observation"]["strictly_after_block"] =
                    serde_json::json!(19)
            }
            9 => {
                bad.cohorts[0].observations[0]
                    .as_object_mut()
                    .unwrap()
                    .remove("process_history_len");
            }
            10 => bad.cohorts[0].observations[0]["process_history_len"] = serde_json::json!(4),
            _ => bad.cohorts[0].first_process_record = 1,
        }
        assert!(
            verify_publication_cohort(&bad, &publication).is_err(),
            "field {field}"
        );
    }
    let mut restarted = evidence.clone();
    let mut next = restarted.feeder_processes[0].clone();
    next.attempt = 2;
    next.pid += 100;
    restarted.feeder_processes[0].stopped_at_millis = Some(2);
    restarted.feeder_processes.push(next);
    // Later teardown/appends cannot invalidate an earlier capture.
    assert!(verify_publication_cohort(&restarted, &publication).is_ok());
    restarted.cohorts[0].observations[0]["process_history_len"] = serde_json::json!(4);
    // A later capture cannot claim that the previous incarnation is current.
    assert!(verify_publication_cohort(&restarted, &publication).is_err());
    restarted.cohorts[0].observations[0]["current_attempts"][0] = serde_json::json!([0, 2, 1100]);
    assert!(verify_publication_cohort(&restarted, &publication).is_ok());
    restarted.cohorts[0].observations[0]["process_history_len"] = serde_json::json!(3);
    assert!(verify_publication_cohort(&restarted, &publication).is_err());
}

fn coen_book(generation: u64, price: &str, volume: &str) -> PriceBook {
    PriceBook {
        generation,
        entries: BTreeMap::from([(
            COEN_USD_SYMBOL.to_owned(),
            PricePoint {
                price: price.to_owned(),
                volume: volume.to_owned(),
            },
        )]),
    }
}

#[test]
fn mock_http_wire_is_exact_and_generation_owned() {
    let mut book = coen_book(7, "1.000000", "1000.000000");
    book.entries.insert(
        "COENUSDC".into(),
        PricePoint {
            price: "1.000001".into(),
            volume: "500.000000".into(),
        },
    );

    assert_eq!(
        mock_response("/api/tickers", COEN_USD_SYMBOL, &book).unwrap(),
        r#"{"data":[{"symbol":"COEN840","price":"1.000000","volume":"1000.000000"}]}"#
    );
    assert_eq!(
        mock_response("/api/candles", COEN_USD_SYMBOL, &book).unwrap(),
        r#"{"data":[]}"#
    );
    assert_eq!(
        mock_response("/api/tickers", "COEN840,COENUSDC", &book).unwrap(),
        r#"{"data":[{"symbol":"COEN840","price":"1.000000","volume":"1000.000000"},{"symbol":"COENUSDC","price":"1.000001","volume":"500.000000"}]}"#
    );
    assert!(mock_response("/api/tickers", "COEN978", &book).is_err());
    assert!(mock_response("/unknown", COEN_USD_SYMBOL, &book).is_err());
    assert_eq!(book.generation, 7);
}

#[test]
fn feeder_config_is_private_and_contains_only_the_frozen_interface() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("feeder.toml");
    let pairs = [FeederPair {
        base: "COEN",
        quote: "840",
        sources: vec![
            FeederSource {
                base: "COEN",
                quote: "USDT",
                price: "1",
                volume: "2",
            },
            FeederSource {
                base: "COEN",
                quote: "USDC",
                price: "1",
                volume: "3",
            },
        ],
    }];
    write_feeder_config(
        &path,
        &FeederConfigInput {
            rpc_url: "http://127.0.0.1:18545",
            chain_id: 54_322_345,
            private_key: "0xsecret",
            validator_address: "0x0000000000000000000000000000000000000001",
            mock_url: "http://127.0.0.1:31415",
            vote_period: 8,
            pairs: &pairs,
        },
    )
    .unwrap();
    let contents = fs::read_to_string(&path).unwrap();
    assert!(contents.contains("vote_period = 8"));
    assert!(contents.contains("poll_interval_secs = 1"));
    assert!(contents.contains("gasless_oracle_votes = true"));
    assert!(contents.contains("enabled = false"));
    assert!(contents.contains("[[api_providers]]"));
    assert!(contents.contains("{ provider = \"mock_http\", base = \"COEN\", quote = \"USDT\" }"));
    assert!(contents.contains("{ provider = \"mock_http\", base = \"COEN\", quote = \"USDC\" }"));
    assert!(!contents.contains("providers ="));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn feeder_restart_does_not_accept_the_previous_log_episodes_start_marker() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("feeder.log");
    fs::write(&path, "starting outbe-feeder\nold episode stopped\n").unwrap();
    let next_episode = fs::metadata(&path).unwrap().len();

    assert!(!log_suffix_contains(
        &path,
        next_episode,
        "starting outbe-feeder"
    ));
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"starting outbe-feeder\n")
        .unwrap();
    assert!(log_suffix_contains(
        &path,
        next_episode,
        "starting outbe-feeder"
    ));
}

#[test]
fn retained_rpc_endpoint_omits_credentials_path_query_and_fragment() {
    assert_eq!(
        sanitize_rpc_endpoint("https://user:pass@rpc.example/v3/secret?key=hidden#fragment"),
        "https://rpc.example"
    );
}

#[test]
fn bound_mock_server_serves_ticker_and_empty_candle_fallback() {
    let mut server = match MockPriceServer::bind(coen_book(3, "1.250000", "4.000000")) {
        Ok(server) => server,
        Err(error) if bind_permission_denied(&error) => return,
        Err(error) => panic!("bind mock server: {error:#}"),
    };
    let ticker = raw_get(server.addr, "/api/tickers?symbols=COEN840");
    assert!(ticker.contains("200 OK"));
    assert!(ticker
        .contains(r#"{"data":[{"symbol":"COEN840","price":"1.250000","volume":"4.000000"}]}"#));
    let candles = raw_get(server.addr, "/api/candles?symbols=COEN840");
    assert!(candles.contains(r#"{"data":[]}"#));
    assert_eq!(server.request_counts(), (1, 1));
    server.stop();
}

#[test]
fn bound_mock_server_publishes_one_atomic_new_price_generation() {
    let mut server = match MockPriceServer::bind(coen_book(3, "1.000000", "4.000000")) {
        Ok(server) => server,
        Err(error) if bind_permission_denied(&error) => return,
        Err(error) => panic!("bind mock server: {error:#}"),
    };

    let generation = server
        .publish(COEN_USD_SYMBOL, "1.080001", "4.000000")
        .expect("publish controlled qualification quote");
    assert_eq!(generation, 4);
    assert_eq!(server.book(), coen_book(4, "1.080001", "4.000000"));
    let ticker = raw_get(server.addr, "/api/tickers?symbols=COEN840");
    assert!(
        ticker
            .contains(r#"{"data":[{"symbol":"COEN840","price":"1.080001","volume":"4.000000"}]}"#),
        "unexpected ticker response: {ticker:?}"
    );
    server.stop();
}

fn bind_permission_denied(error: &eyre::Report) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
    })
}

fn raw_get(addr: SocketAddr, target: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = Vec::new();
    let mut buffer = [0_u8; 4_096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break,
            Err(error) => panic!("read mock response: {error}"),
        }
    }
    String::from_utf8(response).unwrap()
}
