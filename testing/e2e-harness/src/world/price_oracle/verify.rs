//! Post-scenario verification of retained price-oracle evidence.

use super::*;

pub(crate) fn verify_price_oracle_evidence(
    evidence: &PriceOracleEvidenceV1,
    validator_count: usize,
) -> Result<()> {
    eyre::ensure!(
        validator_count > 0,
        "Oracle validator count must be positive"
    );
    eyre::ensure!(
        Path::new(&evidence.feeder_binary)
            .file_name()
            .and_then(|name| name.to_str())
            == Some("outbe-feeder"),
        "Oracle evidence does not identify outbe-feeder"
    );
    eyre::ensure!(
        evidence.feeder_binary_sha256.len() == 64
            && hex::decode(&evidence.feeder_binary_sha256).is_ok(),
        "Oracle evidence has no exact feeder digest"
    );
    eyre::ensure!(
        evidence.ticker_requests > 0 && evidence.candle_requests > 0,
        "Oracle evidence has no provider request activity"
    );
    eyre::ensure!(
        !evidence.feeder_processes.is_empty(),
        "Oracle evidence has no feeder processes"
    );

    let mut attempts = BTreeMap::<usize, Vec<&FeederProcessEvidenceV1>>::new();
    let mut live_pids = std::collections::BTreeSet::new();
    for process in &evidence.feeder_processes {
        eyre::ensure!(
            process.validator_index < validator_count,
            "Oracle feeder references a validator outside the active set"
        );
        eyre::ensure!(
            process.vote_period > 0
                && process.pid > 0
                && process.started_at_millis > 0
                && process
                    .stopped_at_millis
                    .is_none_or(|stopped| stopped >= process.started_at_millis),
            "Oracle feeder lifecycle is malformed"
        );
        eyre::ensure!(
            process.rpc_endpoint.starts_with("http://")
                || process.rpc_endpoint.starts_with("https://"),
            "Oracle feeder RPC endpoint is malformed"
        );
        eyre::ensure!(
            !process.rpc_endpoint.contains(['@', '?', '#']),
            "Oracle feeder RPC endpoint contains credentials or query data"
        );
        eyre::ensure!(
            process.log.ends_with(&format!(
                "validator-{}-feeder-attempt-{}.log",
                process.validator_index, process.attempt
            )),
            "Oracle feeder log is not attempt-specific"
        );
        eyre::ensure!(
            !process.oracle_pairs.is_empty() && !process.source_markets.is_empty(),
            "Oracle feeder has no configured market"
        );
        if process.stopped_at_millis.is_none() {
            eyre::ensure!(
                live_pids.insert(process.pid),
                "Oracle evidence reuses one PID for multiple live feeders"
            );
        }
        attempts
            .entry(process.validator_index)
            .or_default()
            .push(process);
    }
    for (validator_index, processes) in &mut attempts {
        processes.sort_by_key(|process| process.attempt);
        for (offset, process) in processes.iter().enumerate() {
            eyre::ensure!(
                process.attempt == u32::try_from(offset + 1)?,
                "validator-{validator_index} feeder attempts are not monotonic"
            );
            eyre::ensure!(
                (offset + 1 == processes.len()) == process.stopped_at_millis.is_none(),
                "validator-{validator_index} feeder lifecycle has the wrong live attempt"
            );
        }
    }

    eyre::ensure!(
        !evidence.cohorts.is_empty(),
        "used Oracle evidence has no finalized cohort"
    );
    for (ordinal, entry) in evidence.cohorts.iter().enumerate() {
        let end = evidence
            .cohorts
            .get(ordinal + 1)
            .map_or(evidence.feeder_processes.len(), |next| {
                next.first_process_record
            });
        eyre::ensure!(
            entry.first_process_record <= end && end <= evidence.feeder_processes.len(),
            "Oracle cohort process intervals are invalid"
        );
        let mut previous = entry.first_process_record;
        for observation in &entry.observations {
            let captured: usize =
                serde_json::from_value(observation["process_history_len"].clone())?;
            eyre::ensure!(
                captured >= previous && captured <= end,
                "Oracle observation process history regressed or crossed cohorts"
            );
            previous = captured;
        }
        let cohort = &entry.cohort;
        let members = &cohort.members;
        eyre::ensure!(
            members.len() == validator_count
                && cohort.quorum == validator_count - validator_count / 3
                && cohort.checkpoint.block_hash != alloy_primitives::B256::ZERO
                && cohort.checkpoint.state_root != alloy_primitives::B256::ZERO,
            "Oracle cohort lacks canonical membership/checkpoint"
        );
        let identities = members
            .iter()
            .map(|member| member.address)
            .collect::<std::collections::BTreeSet<_>>();
        let ports = members
            .iter()
            .map(|member| member.port)
            .collect::<std::collections::BTreeSet<_>>();
        let indices = members
            .iter()
            .map(|member| member.index)
            .collect::<std::collections::BTreeSet<_>>();
        eyre::ensure!(
            identities.len() == members.len()
                && ports.len() == members.len()
                && indices.len() == members.len()
                && !identities.contains(&alloy_primitives::Address::ZERO),
            "Oracle cohort duplicates an identity or endpoint"
        );
        eyre::ensure!(
            members
                .iter()
                .all(|member| member.node_pid > 0 && member.port > 0)
                && members.windows(2).all(|pair| pair[0].index < pair[1].index),
            "Oracle cohort owners are invalid or unordered"
        );
        let selected = members
            .iter()
            .take(if cohort.overlapping_pairs {
                members.len()
            } else {
                cohort.quorum
            })
            .map(|member| member.index)
            .collect::<Vec<_>>();
        eyre::ensure!(
            cohort.feeder_indices == selected,
            "Oracle cohort has the wrong planned feeders"
        );
        for &(index, attempt, pid) in &entry.current_attempts {
            let member = members
                .iter()
                .find(|member| member.index == index)
                .ok_or_else(|| eyre!("Oracle feeder is not in its cohort"))?;
            eyre::ensure!(
                cohort.feeder_indices.contains(&index)
                    && evidence
                        .feeder_processes
                        .iter()
                        .any(|process| process.validator_index == index
                            && process.attempt == attempt
                            && process.pid == pid
                            && process
                                .validator_address
                                .parse::<alloy_primitives::Address>()
                                .ok()
                                == Some(member.address)),
                "Oracle cohort attempt has no matching public process identity"
            );
        }
    }

    eyre::ensure!(
        !evidence.controlled_sources.is_empty(),
        "Oracle evidence has no independently recorded controlled sources"
    );
    for source in &evidence.controlled_sources {
        eyre::ensure!(
            source.generation > 0
                && source.validator_index < validator_count
                && canonical_unsigned_decimal(&source.price)
                && canonical_unsigned_decimal(&source.volume)
                && !source.oracle_pair.is_empty()
                && source.source_market.starts_with("mock_http:"),
            "Oracle controlled source record is malformed"
        );
    }

    let mut publication_keys = std::collections::BTreeSet::new();
    for publication in &evidence.canonical_publications {
        eyre::ensure!(
            publication.validator_count == validator_count
                && publication
                    .rate
                    .parse::<alloy_primitives::U256>()
                    .is_ok_and(|rate| !rate.is_zero())
                && publication.volume.parse::<alloy_primitives::U256>().is_ok()
                && publication.oracle_block > 0
                && publication.oracle_block <= publication.finalized_height
                && publication.oracle_timestamp > 0
                && publication.oracle_timestamp <= publication.finalized_timestamp
                && publication.age_seconds
                    == publication
                        .finalized_timestamp
                        .saturating_sub(publication.oracle_timestamp)
                && publication.age_seconds <= 21_600,
            "Oracle publication is stale, malformed, or not finalized"
        );
        eyre::ensure!(
            publication_keys.insert((
                publication.phase,
                publication.base.as_str(),
                publication.quote.as_str(),
                publication.oracle_block,
            )),
            "Oracle evidence contains a duplicate publication"
        );
        verify_publication_cohort(evidence, publication)?;
    }
    eyre::ensure!(
        !evidence.canonical_publications.is_empty(),
        "Oracle evidence has no canonical publications"
    );

    Ok(())
}

pub(super) fn verify_publication_cohort(
    evidence: &PriceOracleEvidenceV1,
    publication: &CanonicalPricePublicationV1,
) -> Result<()> {
    let phase = serde_json::to_value(publication.phase)?;
    let mut matched = 0;
    for entry in &evidence.cohorts {
        for wrapper in &entry.observations {
            let observation = &wrapper["observation"];
            if !verifies_publication(observation, &phase, publication) {
                continue;
            }
            matched += 1;
            let checkpoint: OracleCheckpointV1 =
                serde_json::from_value(observation["checkpoint"].clone())?;
            let after = observation["strictly_after_block"]
                .as_u64()
                .ok_or_else(|| eyre!("Oracle proof has no publication lower bound"))?;
            eyre::ensure!(
                checkpoint.height == publication.finalized_height
                    && checkpoint.height >= entry.cohort.checkpoint.height
                    && after >= entry.cohort.checkpoint.height
                    && checkpoint.block_hash != alloy_primitives::B256::ZERO
                    && checkpoint.state_root != alloy_primitives::B256::ZERO
                    && publication.oracle_block > after,
                "Oracle proof checkpoint/advance mismatch"
            );
            let attempts: Vec<(usize, u32, u32)> =
                serde_json::from_value(wrapper["current_attempts"].clone())?;
            let history_len: usize =
                serde_json::from_value(wrapper["process_history_len"].clone())?;
            eyre::ensure!(
                entry.first_process_record <= history_len
                    && history_len <= evidence.feeder_processes.len(),
                "Oracle proof has invalid process history bounds"
            );
            validate_live_feeder_indices(
                &entry.cohort,
                &attempts.iter().map(|attempt| attempt.0).collect::<Vec<_>>(),
            )?;
            for (index, attempt, pid) in attempts {
                let member = entry
                    .cohort
                    .members
                    .iter()
                    .find(|member| member.index == index)
                    .ok_or_else(|| eyre!("Oracle proof has an unknown feeder owner"))?;
                let (record_index, process) = evidence.feeder_processes[..history_len]
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, process)| process.validator_index == index)
                    .ok_or_else(|| eyre!("Oracle proof feeder has no captured launch record"))?;
                eyre::ensure!(
                    record_index >= entry.first_process_record
                        && process.attempt == attempt
                        && process.pid == pid
                        && process
                            .validator_address
                            .parse::<alloy_primitives::Address>()?
                            == member.address,
                    "Oracle proof feeder is not the captured cohort incarnation"
                );
            }
            let reads = observation["reads"]
                .as_array()
                .ok_or_else(|| eyre!("Oracle proof has no pinned peer reads"))?;
            eyre::ensure!(
                reads.len() == entry.cohort.members.len(),
                "Oracle proof omitted a cohort peer"
            );
            for (read, member) in reads.iter().zip(&entry.cohort.members) {
                let point: OracleCheckpointV1 = serde_json::from_value(read["checkpoint"].clone())?;
                let rate: alloy_primitives::U256 = serde_json::from_value(read["rate"].clone())?;
                let volume: alloy_primitives::U256 =
                    serde_json::from_value(read["volume"].clone())?;
                eyre::ensure!(
                    read["port"].as_u64() == Some(u64::from(member.port))
                        && read["finalized"]
                            .as_u64()
                            .is_some_and(|height| height >= point.height)
                        && point == checkpoint
                        && rate == publication.rate.parse::<alloy_primitives::U256>()?
                        && volume == publication.volume.parse::<alloy_primitives::U256>()?
                        && read["oracle_block"].as_u64() == Some(publication.oracle_block)
                        && read["oracle_timestamp"].as_u64() == Some(publication.oracle_timestamp)
                        && read["finalized_timestamp"].as_u64()
                            == Some(publication.finalized_timestamp),
                    "Oracle proof peer state/finality does not match publication"
                );
            }
        }
    }
    eyre::ensure!(
        matched == 1,
        "Oracle publication requires exactly one complete cohort proof"
    );
    Ok(())
}

fn canonical_unsigned_decimal(value: &str) -> bool {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    !whole.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
        && value.matches('.').count() <= 1
}

fn verifies_publication(
    observation: &serde_json::Value,
    phase: &serde_json::Value,
    publication: &CanonicalPricePublicationV1,
) -> bool {
    let same_pair = observation["base"].as_str() == Some(publication.base.as_str())
        && observation["quote"].as_str() == Some(publication.quote.as_str());
    observation["kind"].as_str() == Some("publication_verified")
        && observation["phase"] == *phase
        && same_pair
        && observation["oracle_block"].as_u64() == Some(publication.oracle_block)
}
