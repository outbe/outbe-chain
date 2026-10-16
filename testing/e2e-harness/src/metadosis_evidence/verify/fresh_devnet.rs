//! The fresh-devnet process scenario and its retained Oracle and genesis artifacts.

use super::*;

pub(super) fn verify_fresh_devnet_process(
    bundle_root: &Path,
    receipt: &TestRunReceiptV1,
) -> Result<Vec<MemberDigestV1>> {
    ensure!(
        receipt.execution.succeeded(),
        "fresh-devnet process command did not succeed"
    );
    let root = bundle_root.join("fresh-devnet");
    let mut members = Vec::new();
    let mut scenarios = Vec::new();
    for entry in WalkDir::new(&root).follow_links(false) {
        let entry = entry.wrap_err("walk retained fresh-devnet artifacts")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(bundle_root)
            .wrap_err("fresh-devnet member escaped bundle")?;
        let relative = slash_path(relative)?;
        if relative.starts_with("fresh-devnet/evidence/scenario-") && relative.ends_with(".json") {
            scenarios.push(relative.clone());
        }
        members.push(digest_member(bundle_root, &relative)?);
    }
    members.sort();
    ensure!(
        scenarios.len() == 1,
        "fresh-devnet process must retain exactly one scenario receipt"
    );
    let scenario: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle_root.join(&scenarios[0]))?)
            .wrap_err("decode fresh-devnet scenario receipt")?;
    ensure!(
        scenario["feature"] == "Off-chain computation and Metadosis"
            && scenario["scenario"]
                == "A public Tribute completes real OCOMP, FullNode verification, NOD, and replay"
            && scenario["result"] == "passed",
        "fresh-devnet scenario identity or verdict mismatch"
    );
    let expected_source_sha = receipt_source_sha(bundle_root)?;
    ensure!(
        scenario["source"]["sha"].as_str() == Some(expected_source_sha.as_str())
            && scenario["source"]["tracked_dirty"] == false
            && scenario["source"]["untracked_dirty"] == false,
        "fresh-devnet scenario source identity mismatch"
    );
    ensure!(
        scenario["environment"]["validators"] == 4
            && scenario["environment"]["tee"] == "gramine-direct"
            && scenario["environment"]["all"] == true,
        "fresh-devnet scenario did not use the required four-validator Linux topology"
    );
    let launch = &scenario["ocomp"]["topology"]["launch_identity"];
    ensure!(
        launch["classification"] == "measurement"
            && launch["activation_height"] == 1
            && launch["metadosis_storage_layout_hash"]
                == crate::world::ocomp::METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX
            && launch["fork_install_hash"]
                .as_str()
                .is_some_and(|hash| hash.len() == 66)
            && launch["protocol_bundle_hash"]
                .as_str()
                .is_some_and(|hash| hash.len() == 66),
        "fresh-devnet launch identity is not Measurement@1 with the pinned Metadosis layout"
    );
    let protocol_parameters = verify_fresh_artifacts(bundle_root, &scenario, &members)?;
    #[cfg(feature = "ocomp-integration")]
    verify_price_oracle_evidence(&scenario)?;
    let processes = scenario["ocomp"]["topology"]["processes"]
        .as_array()
        .ok_or_else(|| eyre::eyre!("fresh-devnet scenario has no process topology"))?;
    ensure!(
        processes.iter().all(|process| matches!(
            process["role"].as_str(),
            Some("snapshot_exporter" | "worker")
        )),
        "fresh-devnet process topology contains a non-external OCOMP role"
    );
    for validator_index in 0..4_u64 {
        for role in ["snapshot_exporter", "worker"] {
            ensure!(
                processes.iter().any(|process| {
                    process["validator_index"].as_u64() == Some(validator_index)
                        && process["role"] == role
                        && process["stopped_at_millis"].is_null()
                }),
                "fresh-devnet validator {validator_index} has no live {role}"
            );
        }
    }
    let public = &scenario["ocomp"]["public_path"];
    let capacity = &public["capacity_public_path"];
    ensure!(
        public["atomic_quorum_apply_verified"] == true
            && public["activation"].is_object()
            && public["certified_generation"].is_object()
            && capacity["q_forming_receipt_success"] == true
            && capacity["canonical_import_verified"] == true
            && capacity["canonical_import_validator_count"] == 4
            && capacity["tribute_count"] == 257
            && capacity["nod_count"] == 257,
        "fresh-devnet scenario did not close the public READY/OCOMP/terminal path"
    );
    let lifecycle = &public["metadosis_fresh_lifecycle"];
    let worldwide_day = lifecycle["worldwide_day"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no WorldwideDay"))?;
    let forming_start = lifecycle["forming_start"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no forming_start"))?;
    let forming_end = lifecycle["forming_end"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no forming_end"))?;
    let lookback_end = lifecycle["lookback_end"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no lookback_end"))?;
    let offering_end = lifecycle["offering_end"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no offering_end"))?;
    let scheduled = lifecycle["scheduled_process_time"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no processing time"))?;
    ensure!(
        forming_end.checked_sub(forming_start)
            == Some(protocol_parameters.metadosis_forming_period_seconds)
            && lookback_end.checked_sub(forming_end)
                == Some(protocol_parameters.metadosis_lookback_delay_seconds)
            && offering_end.checked_sub(lookback_end)
                == Some(protocol_parameters.metadosis_offering_period_seconds)
            && scheduled.checked_sub(offering_end)
                == Some(protocol_parameters.metadosis_waiting_period_seconds),
        "fresh Metadosis lifecycle differs from immutable genesis phase durations"
    );
    let initial_timestamp = lifecycle["initial_timestamp"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no initial timestamp"))?;
    ensure!(
        initial_timestamp >= forming_start
            && initial_timestamp < forming_end
            && lifecycle["initial_unix_time_offset_secs"].is_i64()
            && crate::world::localnet::ymd_utc(initial_timestamp)
                .parse::<u64>()
                .ok()
                == Some(worldwide_day)
            && initial_timestamp
                .checked_add(14 * 3_600)
                .map(crate::world::localnet::ymd_utc)
                .and_then(|day| day.parse::<u64>().ok())
                == Some(worldwide_day),
        "fresh Metadosis lifecycle did not begin inside FORMING on the block-1 UTC/UTC+14 WWD"
    );
    ensure!(
        lifecycle["genesis_hash"] == launch["genesis_hash"]
            && lifecycle["created_validator_count"] == 4
            && lifecycle["unknown_status_revert_validator_count"] == 4
            && lifecycle["offering_validator_count"] == 4
            && lifecycle["ready_validator_count"] == 4
            && lifecycle["completed_validator_count"] == 4,
        "fresh Metadosis lifecycle is not bound to one four-validator genesis"
    );
    let started = &lifecycle["started"];
    ensure!(
        started["worldwide_day"].as_u64() == Some(worldwide_day)
            && started["block_number"] == 1
            && started["forming_start"].as_u64() == Some(forming_start)
            && started["forming_end"].as_u64() == Some(forming_end)
            && started["lookback_end"].as_u64() == Some(lookback_end)
            && started["offering_end"].as_u64() == Some(offering_end)
            && started["scheduled_process_time"].as_u64() == Some(scheduled)
            && digest_like(&started["block_hash"]),
        "fresh Metadosis lifecycle has no canonical block-1 Create evidence"
    );
    let changes = lifecycle["status_changes"]
        .as_array()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no status changes"))?;
    let expected_edges = [(0_u64, 1_u64), (1, 2), (2, 3), (3, 4)];
    ensure!(
        changes.len() == expected_edges.len(),
        "fresh Metadosis lifecycle has an incomplete Advance edge set"
    );
    for (change, (old_status, new_status)) in changes.iter().zip(expected_edges) {
        ensure!(
            change["worldwide_day"].as_u64() == Some(worldwide_day)
                && change["old_status"].as_u64() == Some(old_status)
                && change["new_status"].as_u64() == Some(new_status)
                && change["block_number"]
                    .as_u64()
                    .is_some_and(|height| height >= 2)
                && digest_like(&change["block_hash"]),
            "fresh Metadosis lifecycle contains an invalid finalized Advance edge"
        );
    }
    let epochs = lifecycle["time_control_epochs"]
        .as_array()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no time-control history"))?;
    ensure!(
        epochs.len() == 2
            && epochs[0]["requested_timestamp"].as_u64() == forming_end.checked_add(1)
            && epochs[1]["requested_timestamp"].as_u64() == scheduled.checked_add(1),
        "fresh Metadosis lifecycle has incorrect logical-time boundaries"
    );
    let mut previous_after_height = 0;
    for epoch in epochs {
        ensure!(
            epoch["unix_time_offset_secs"].is_i64(),
            "fresh Metadosis time-control epoch has no signed offset"
        );
        let before = verified_time_control_points(&epoch["before_restart"])?;
        let after = verified_time_control_points(&epoch["after_restart"])?;
        ensure!(
            before[0] >= previous_after_height && after[0] > before[0],
            "fresh Metadosis time-control epoch breaks same-chain finality continuity"
        );
        previous_after_height = after[0];
    }
    ensure!(
        public["job_request"]["worldwide_day"].as_u64() == Some(worldwide_day)
            && public["activation"]["worldwide_day"].as_u64() == Some(worldwide_day)
            && digest_like(&public["activation"]["terminal_receipt_hash"])
            && public["certified_generation"]["worldwide_day"].as_u64() == Some(worldwide_day),
        "fresh Metadosis terminal evidence is not linked to the runtime-created WWD"
    );
    for required in [
        "fresh-devnet/commands.log",
        "fresh-devnet/run.log",
        scenarios[0].as_str(),
    ] {
        ensure!(
            members.iter().any(|member| member.path == required),
            "fresh-devnet bundle is missing {required}"
        );
    }
    Ok(members)
}

#[cfg(feature = "ocomp-integration")]
pub(super) fn verify_price_oracle_evidence(scenario: &serde_json::Value) -> Result<()> {
    let validator_count = scenario["environment"]["validators"]
        .as_u64()
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| eyre::eyre!("fresh-devnet evidence has no validator count"))?;
    let evidence: crate::world::price_oracle::PriceOracleEvidenceV1 =
        serde_json::from_value(scenario["price_oracle"].clone())
            .wrap_err("decode typed fresh-devnet PriceOracleEvidenceV1")?;
    crate::world::price_oracle::verify_price_oracle_evidence(&evidence, validator_count)?;

    ensure!(
        validator_count == 4,
        "fresh-devnet Oracle evidence requires four validators"
    );
    let quorum = validator_count - validator_count / 3;
    ensure!(
        evidence.feeder_processes.len() == quorum * 3,
        "fresh-devnet Oracle requires the initial quorum and two clock restarts"
    );
    let mut identities = BTreeMap::new();
    let mut previous_stops = BTreeMap::new();
    for attempt in 1..=3 {
        let mut validator_indexes = BTreeSet::new();
        let mut validator_addresses = BTreeSet::new();
        let mut pids = BTreeSet::new();
        for process in evidence
            .feeder_processes
            .iter()
            .filter(|row| row.attempt == attempt)
        {
            let address = process
                .validator_address
                .parse::<alloy_primitives::Address>()
                .wrap_err("decode fresh-devnet Oracle validator address")?;
            ensure!(
                (attempt == 3) == process.stopped_at_millis.is_none()
                    && process.oracle_pairs == ["COEN/840"]
                    && process.source_markets == ["mock_http:COEN/840"]
                    && !address.is_zero()
                    && validator_indexes.insert(process.validator_index)
                    && validator_addresses.insert(address)
                    && pids.insert(process.pid),
                "fresh-devnet Oracle evidence does not identify independent quorum feeders"
            );
            let identity = (address, process.rpc_endpoint.as_str(), process.vote_period);
            if attempt == 1 {
                identities.insert(process.validator_index, identity);
            } else {
                ensure!(
                    identities.get(&process.validator_index) == Some(&identity)
                        && previous_stops
                            .get(&process.validator_index)
                            .is_some_and(|stop| *stop <= process.started_at_millis),
                    "fresh-devnet Oracle restart changed identity or overlapped its old process"
                );
            }
            if let Some(stop) = process.stopped_at_millis {
                previous_stops.insert(process.validator_index, stop);
            }
        }
        ensure!(
            validator_indexes == (0..quorum).collect(),
            "fresh-devnet Oracle attempt has the wrong independent quorum"
        );
    }
    let validator_indexes = identities.keys().copied().collect::<BTreeSet<_>>();

    let source_rows = evidence
        .controlled_sources
        .iter()
        .map(|source| {
            (
                source.validator_index,
                source.phase,
                source.generation,
                source.oracle_pair.as_str(),
                source.source_market.as_str(),
                source.price.as_str(),
                source.volume.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    ensure!(
        evidence.controlled_sources.len() == quorum * 4
            && source_rows.len() == quorum * 3
            && validator_indexes.iter().all(|validator_index| {
                source_rows.contains(&(
                    *validator_index,
                    crate::world::price_oracle::OracleEvidencePhaseV1::Initial,
                    1,
                    "COEN/840",
                    "mock_http:COEN/840",
                    "1.000000",
                    "1000.000000",
                ))
            }),
        "fresh-devnet Oracle evidence is not bound to the initial controlled sources"
    );
    for validator_index in &validator_indexes {
        let restarts = evidence
            .controlled_sources
            .iter()
            .filter(|source| {
                source.validator_index == *validator_index
                    && source.phase
                        == crate::world::price_oracle::OracleEvidencePhaseV1::ClockRestart
            })
            .collect::<Vec<_>>();
        ensure!(
            restarts.len() == 2
                && restarts.iter().all(|source| {
                    source.generation == 1
                        && source.oracle_pair == "COEN/840"
                        && source.source_market == "mock_http:COEN/840"
                        && source.price == "1.000000"
                        && source.volume == "1000.000000"
                }),
            "fresh-devnet Oracle sources do not cover both clock restarts"
        );
    }
    let updates = evidence
        .controlled_sources
        .iter()
        .filter(|source| {
            source.phase == crate::world::price_oracle::OracleEvidencePhaseV1::ControlledUpdate
        })
        .collect::<Vec<_>>();
    let final_price = updates
        .first()
        .ok_or_else(|| eyre::eyre!("fresh-devnet Oracle evidence has no controlled update"))?
        .price
        .as_str();
    let final_rate = scale6_decimal_minor(final_price)?;
    let update_validator_indexes = updates
        .iter()
        .map(|source| source.validator_index)
        .collect::<BTreeSet<_>>();
    ensure!(
        updates.len() == quorum
            && update_validator_indexes == validator_indexes
            && final_rate > 1_000_000
            && updates.iter().all(|source| {
                validator_indexes.contains(&source.validator_index)
                    && source.generation == 2
                    && source.oracle_pair == "COEN/840"
                    && source.source_market == "mock_http:COEN/840"
                    && source.price == final_price
                    && source.volume == "1000.000000"
            }),
        "fresh-devnet Oracle evidence is not bound to one controlled update"
    );

    let expected_phases = [
        crate::world::price_oracle::OracleEvidencePhaseV1::Initial,
        crate::world::price_oracle::OracleEvidencePhaseV1::ClockRestart,
        crate::world::price_oracle::OracleEvidencePhaseV1::ClockRestart,
        crate::world::price_oracle::OracleEvidencePhaseV1::ControlledUpdate,
    ];
    let expected_base = format!("{:#x}", alloy_primitives::Address::ZERO);
    let expected_quote = format!("{:#x}", outbe_primitives::asset_type::currency_address(840));
    ensure!(
        evidence.cohorts.len() == 3,
        "fresh-devnet Oracle needs exactly three launch cohorts"
    );
    for (index, entry) in evidence.cohorts.iter().enumerate() {
        ensure!(
            entry.first_process_record == index * quorum
                && entry.phase == expected_phases[index]
                && !entry.cohort.overlapping_pairs,
            "fresh-devnet Oracle cohort does not match its launch stage"
        );
    }
    let mut previous_oracle_block = 0;
    ensure!(
        evidence.canonical_publications.len() == expected_phases.len(),
        "fresh-devnet Oracle evidence has the wrong publication count"
    );
    for (index, publication) in evidence.canonical_publications.iter().enumerate() {
        let expected_rate = if index + 1 == expected_phases.len() {
            final_rate
        } else {
            1_000_000
        };
        ensure!(
            publication.phase == expected_phases[index]
                && publication.base == expected_base
                && publication.quote == expected_quote
                && publication.rate == expected_rate.to_string()
                && publication.volume == "1000000000"
                && publication.oracle_block > previous_oracle_block,
            "fresh-devnet Oracle evidence has a divergent canonical publication"
        );
        previous_oracle_block = publication.oracle_block;
        let cohort_index = index.min(2);
        let entry = &evidence.cohorts[cohort_index];
        let matching = entry
            .observations
            .iter()
            .find(|wrapper| {
                let observation = &wrapper["observation"];
                let verified = observation["kind"].as_str() == Some("publication_verified")
                    && observation["oracle_block"].as_u64() == Some(publication.oracle_block);
                let same_pair = observation["phase"] == serde_json::json!(publication.phase)
                    && observation["base"].as_str() == Some(publication.base.as_str())
                    && observation["quote"].as_str() == Some(publication.quote.as_str());
                verified && same_pair
            })
            .ok_or_else(|| eyre::eyre!("fresh-devnet publication has no matching launch cohort"))?;
        let attempts: Vec<(usize, u32, u32)> =
            serde_json::from_value(matching["current_attempts"].clone())?;
        ensure!(
            attempts.len() == quorum
                && attempts.iter().all(|(_, attempt, _)| {
                    usize::try_from(*attempt).ok() == Some(cohort_index + 1)
                }),
            "fresh-devnet publication references the wrong feeder restart"
        );
    }
    ensure!(
        evidence.quorum_loss_windows.is_empty() && evidence.penalty_snapshots.is_empty(),
        "fresh-devnet Oracle evidence mixes another scenario profile"
    );
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
pub(super) fn scale6_decimal_minor(value: &str) -> Result<u128> {
    let (whole, fraction) = value
        .split_once('.')
        .ok_or_else(|| eyre::eyre!("controlled Oracle price is not a scale-6 decimal"))?;
    ensure!(
        fraction.len() == 6
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.bytes().all(|byte| byte.is_ascii_digit()),
        "controlled Oracle price is not a canonical scale-6 decimal"
    );
    whole
        .parse::<u128>()?
        .checked_mul(1_000_000)
        .and_then(|scaled| scaled.checked_add(fraction.parse::<u128>().ok()?))
        .ok_or_else(|| eyre::eyre!("controlled Oracle price overflows scale-6 evidence"))
}

#[cfg(feature = "ocomp-integration")]
pub(super) fn verify_fresh_artifacts(
    bundle_root: &Path,
    scenario: &serde_json::Value,
    members: &[MemberDigestV1],
) -> Result<GenesisProtocolParametersV1> {
    let exact_binaries = scenario["ocomp"]["exact_binaries"]
        .as_object()
        .ok_or_else(|| eyre::eyre!("fresh-devnet scenario has no exact binary digests"))?;
    let binaries = [
        ("outbe_chain", "outbe-chain"),
        ("outbe_ocomp", "outbe-ocomp"),
        ("outbe_feeder", "outbe-feeder"),
        ("outbe_cli", "outbe-cli"),
        ("outbe_keygen", "outbe-keygen"),
        ("outbe_e2e", "outbe-e2e"),
        ("outbe_tee_enclave", "outbe-tee-enclave"),
    ];
    ensure!(
        exact_binaries.len() == binaries.len(),
        "fresh-devnet scenario has an unexpected exact binary set"
    );
    for (identity, file_name) in binaries {
        let claim: MemberDigestV1 = serde_json::from_value(
            exact_binaries
                .get(identity)
                .cloned()
                .ok_or_else(|| eyre::eyre!("fresh-devnet scenario is missing {identity}"))?,
        )?;
        ensure!(
            Path::new(&claim.path)
                .file_name()
                .and_then(|name| name.to_str())
                == Some(file_name),
            "fresh-devnet scenario {identity} path does not identify {file_name}"
        );
        let relative = format!("fresh-devnet/artifacts/{file_name}");
        let retained = members
            .iter()
            .find(|member| member.path == relative)
            .ok_or_else(|| eyre::eyre!("fresh-devnet bundle is missing {relative}"))?;
        ensure!(
            claim.length == retained.length && claim.sha256 == retained.sha256,
            "fresh-devnet retained {file_name} differs from the executed binary"
        );
    }

    let genesis_relative = "fresh-devnet/artifacts/genesis.json";
    ensure!(
        members.iter().any(|member| member.path == genesis_relative),
        "fresh-devnet bundle is missing {genesis_relative}"
    );
    let genesis_path = bundle_root.join(genesis_relative);
    let genesis: serde_json::Value = serde_json::from_slice(&std::fs::read(&genesis_path)?)
        .wrap_err("decode retained fresh genesis")?;
    let protocol_parameters = GenesisProtocolParametersV1::from_genesis(&genesis)
        .wrap_err("read protocol parameters from retained fresh genesis")?;
    let chain_spec = reth_ethereum::cli::chainspec::chain_value_parser(
        genesis_path
            .to_str()
            .ok_or_else(|| eyre::eyre!("fresh genesis path is not UTF-8"))?,
    )?
    .as_ref()
    .clone()
    .map_header(OutbeHeader::new);
    let genesis_hash = chain_spec.genesis_hash();
    let launch = &scenario["ocomp"]["topology"]["launch_identity"];
    ensure!(
        launch["genesis_hash"].as_str() == Some(format!("{genesis_hash:#x}").as_str()),
        "fresh-devnet launch genesis hash is not derived from retained genesis bytes"
    );

    let manifest = &genesis["config"]["ocompForkInstallV1"];
    let canonical_hex = manifest["canonicalBytes"]
        .as_str()
        .and_then(|value| value.strip_prefix("0x"))
        .ok_or_else(|| eyre::eyre!("retained fresh genesis has no canonical OCOMP install"))?;
    let canonical_install = hex::decode(canonical_hex)?;
    let install_relative = "fresh-devnet/artifacts/fork-install-v1.ocb1";
    ensure!(
        std::fs::read(bundle_root.join(install_relative))? == canonical_install,
        "retained fresh fork install differs from genesis manifest bytes"
    );
    let limits = poc_schema_limits();
    let install = OcompForkInstallV1::decode_canonical(&canonical_install, &limits)?;
    install.validate_for_chain(chain_spec.chain().id(), genesis_hash, &limits)?;
    let install_hash = install.install_hash(&limits)?;
    let protocol_bundle_hash = install.protocol_bundle.protocol_bundle_hash(&limits)?;
    ensure!(
        install.classification == OcompForkInstallClassification::Measurement
            && install.activation_height == 1
            && launch["chain_id"].as_u64() == Some(install.request_profile.chain_id)
            && launch["fork_install_hash"].as_str() == Some(format!("{install_hash:#x}").as_str())
            && manifest["installHash"].as_str() == Some(format!("{install_hash:#x}").as_str())
            && launch["protocol_bundle_hash"].as_str()
                == Some(format!("{protocol_bundle_hash:#x}").as_str())
            && install.request_profile.genesis_hash == genesis_hash,
        "fresh-devnet launch identity is not derived from retained install/profile bytes"
    );
    ensure!(
        genesis["config"]["metadosisStorageLayoutV1"]["layoutHash"]
            == launch["metadosis_storage_layout_hash"],
        "fresh-devnet layout identity differs from retained genesis"
    );

    for (relative, expected) in [
        (
            "fresh-devnet/artifacts/protocol-bundle-v1.ocb1",
            install.protocol_bundle.encode_canonical(&limits)?,
        ),
        (
            "fresh-devnet/artifacts/capacity-profile-v1.ocb1",
            install
                .request_profile
                .capacity_profile
                .encode_canonical(&limits)?,
        ),
    ] {
        ensure!(
            members.iter().any(|member| member.path == relative)
                && std::fs::read(bundle_root.join(relative))? == expected,
            "fresh-devnet retained profile member {relative} is missing or mismatched"
        );
    }
    Ok(protocol_parameters)
}

#[cfg(not(feature = "ocomp-integration"))]
pub(super) fn verify_fresh_artifacts(
    _bundle_root: &Path,
    _scenario: &serde_json::Value,
    _members: &[MemberDigestV1],
) -> Result<GenesisProtocolParametersV1> {
    eyre::bail!("fresh-devnet artifact verification requires the ocomp-integration feature");
}

pub(super) fn digest_like(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|digest| {
        digest.len() == 66
            && digest.starts_with("0x")
            && hex::decode(&digest[2..])
                .is_ok_and(|bytes| bytes.len() == 32 && bytes.iter().any(|byte| *byte != 0))
    })
}

pub(super) fn verified_time_control_points(value: &serde_json::Value) -> Result<[u64; 2]> {
    let points = value
        .as_array()
        .ok_or_else(|| eyre::eyre!("time-control finalized points are not an array"))?;
    ensure!(
        points.len() == 4,
        "time-control epoch must retain four validator points"
    );
    let first_height = points[0]["block_number"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("time-control point has no block number"))?;
    let first_timestamp = points[0]["block_timestamp"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("time-control point has no block timestamp"))?;
    let first_hash = &points[0]["block_hash"];
    ensure!(
        digest_like(first_hash),
        "time-control point has no canonical block hash"
    );
    let mut validators = BTreeSet::new();
    for point in points {
        let validator = point["validator_index"]
            .as_u64()
            .ok_or_else(|| eyre::eyre!("time-control point has no validator index"))?;
        ensure!(
            validator < 4 && validators.insert(validator),
            "time-control validator index is duplicate or outside the committee"
        );
        ensure!(
            point["block_number"].as_u64() == Some(first_height)
                && point["block_timestamp"].as_u64() == Some(first_timestamp)
                && point["block_hash"] == *first_hash,
            "time-control validators did not converge on one finalized point"
        );
    }
    Ok([first_height, first_timestamp])
}
