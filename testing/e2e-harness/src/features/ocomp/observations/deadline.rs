use crate::features::ocomp::*;

pub(in crate::features::ocomp) fn accountability_slot_for_vote(
    accountability: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
    vote: &ResultVoteV1,
) -> Option<u16> {
    let mut matches = accountability
        .slot_first_signatures
        .iter()
        .filter(|(_, signature)| signature.as_slice() == vote.signature_rs)
        .map(|(validator_index, _)| *validator_index);
    let participant_index = matches.next()?;
    matches.next().is_none().then_some(participant_index)
}

// Canonical policy: crates/system/validatorset/src/runtime.rs. Not a test override.
pub(in crate::features::ocomp) const DYNAMIC_OCOMP_RECOVERY_BLOCKS: u64 = 43_200;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(in crate::features::ocomp) struct DynamicDeadlineAccount {
    pub(in crate::features::ocomp) bonded: U256,
    pub(in crate::features::ocomp) mirrored: U256,
    pub(in crate::features::ocomp) total_staked: U256,
    pub(in crate::features::ocomp) staking_balance: U256,
    pub(in crate::features::ocomp) status: u8,
    pub(in crate::features::ocomp) ordinary_slash_count: u64,
    pub(in crate::features::ocomp) ocomp_miss_count: u64,
    pub(in crate::features::ocomp) ocomp_recovery_deadline: u64,
    pub(in crate::features::ocomp) active: Vec<Address>,
    pub(in crate::features::ocomp) participants: Vec<Address>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(in crate::features::ocomp) struct DynamicDeadlineMiss {
    pub(in crate::features::ocomp) validator: Address,
    pub(in crate::features::ocomp) job_id: B256,
    pub(in crate::features::ocomp) miss_count: u64,
    pub(in crate::features::ocomp) slashed_bonded: U256,
    pub(in crate::features::ocomp) recovery_deadline: u64,
    pub(in crate::features::ocomp) first_in_window: bool,
    pub(in crate::features::ocomp) height: u64,
    pub(in crate::features::ocomp) block_hash: B256,
    pub(in crate::features::ocomp) transaction_hash: B256,
    pub(in crate::features::ocomp) log_index: u64,
}

pub(in crate::features::ocomp) fn dynamic_deadline_ports(
    mut founders: Vec<u16>,
    joiner: u16,
) -> eyre::Result<Vec<u16>> {
    ensure!(founders.len() == 4, "four expected founder observers");
    founders.push(joiner);
    ensure!(
        founders
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == 5,
        "five distinct observers including validator 3 and the joiner"
    );
    Ok(founders)
}

pub(in crate::features::ocomp) fn dynamic_deadline_assert_live(
    world: &mut World,
    owned: &[(u32, u32)],
) {
    assert_eq!(owned.len(), 5, "five expected owned process pairs");
    for (index, expected) in owned.iter().enumerate() {
        assert_eq!(
            world
                .localnet
                .live_validator_and_enclave_pids(index)
                .expect("dynamic deadline observer exited or is unobservable"),
            *expected,
            "dynamic deadline observer changed incarnation"
        );
    }
}

pub(in crate::features::ocomp) fn dynamic_deadline_checkpoint(
    world: &World,
    ports: &[u16],
    height: u64,
) -> eyre::Result<crate::world::rpc::FinalizedCheckpoint> {
    let observed = ports
        .iter()
        .map(|&port| {
            Ok((
                port,
                world.rpc.finalized_result(port)?,
                world.rpc.checkpoint_at(port, height)?,
            ))
        })
        .collect::<eyre::Result<Vec<_>>>()?;
    dynamic_deadline_validate_checkpoints(ports, height, &observed)
}

pub(in crate::features::ocomp) fn dynamic_deadline_validate_checkpoints(
    ports: &[u16],
    height: u64,
    observed: &[(u16, u64, crate::world::rpc::FinalizedCheckpoint)],
) -> eyre::Result<crate::world::rpc::FinalizedCheckpoint> {
    ensure!(
        ports.len() == 5
            && ports
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == 5,
        "five distinct expected finalized observers required"
    );
    ensure!(
        observed.len() == ports.len(),
        "a finalized observer is missing"
    );
    let expected = observed[0].2;
    ensure!(expected.height == height, "wrong pinned checkpoint height");
    for (&port, &(actual_port, finalized, checkpoint)) in ports.iter().zip(observed) {
        ensure!(port == actual_port, "wrong finalized observer identity");
        ensure!(finalized >= height, "observer has not finalized h{height}");
        ensure!(
            checkpoint == expected,
            "finalized hash/root mismatch at h{height}"
        );
    }
    Ok(expected)
}

pub(in crate::features::ocomp) fn dynamic_deadline_account(
    world: &World,
    port: u16,
    victim: Address,
    height: u64,
) -> eyre::Result<DynamicDeadlineAccount> {
    use crate::internal::addresses::{STK_ADDR, VS_ADDR};
    let url = world.rpc.url(port);
    let record = eth::read_call_at_result(
        &url,
        VS_ADDR,
        &eth::IValidatorSet::validatorByAddressCall { addr: victim },
        height,
    )
    .map_err(|e| eyre!(e))?;
    ensure!(
        record.validatorAddress == victim,
        "wrong validator accounting identity"
    );
    let mut active = eth::read_call_at_result(
        &url,
        VS_ADDR,
        &eth::IValidatorSet::getActiveValidatorsCall {},
        height,
    )
    .map_err(|e| eyre!(e))?;
    let mut participants = eth::read_call_at_result(
        &url,
        VS_ADDR,
        &eth::IValidatorSet::getActiveConsensusSetCall {},
        height,
    )
    .map_err(|e| eyre!(e))?;
    active.sort_unstable();
    participants.sort_unstable();
    let recovery_word = |base: u64| -> eyre::Result<u64> {
        // ValidatorSet schema slots 61/62 use this same production mapping helper.
        let slot = outbe_primitives::storage::StorageKey::mapping_slot(&victim, U256::from(base));
        let value = eth::raw_json_result(
            &url,
            "eth_getStorageAt",
            serde_json::json!([VS_ADDR, format!("0x{slot:x}"), format!("0x{height:x}")]),
        )?;
        dynamic_deadline_storage_u64(&value)
    };
    Ok(DynamicDeadlineAccount {
        bonded: eth::read_call_at_result(
            &url,
            STK_ADDR,
            &eth::IStaking::getStakeCall { validator: victim },
            height,
        )
        .map_err(|e| eyre!(e))?,
        mirrored: record.stake,
        total_staked: eth::read_call_at_result(
            &url,
            STK_ADDR,
            &eth::IStaking::getTotalStakedCall {},
            height,
        )
        .map_err(|e| eyre!(e))?,
        staking_balance: serde_json::from_value(
            eth::raw_json_result(
                &url,
                "eth_getBalance",
                serde_json::json!([STK_ADDR, format!("0x{height:x}")]),
            )
            .map_err(|e| eyre!(e))?,
        )?,
        status: record.status,
        ordinary_slash_count: record.slashCount,
        ocomp_miss_count: recovery_word(61)?,
        ocomp_recovery_deadline: recovery_word(62)?,
        active,
        participants,
    })
}

pub(in crate::features::ocomp) fn dynamic_deadline_storage_u64(
    value: &serde_json::Value,
) -> eyre::Result<u64> {
    // eth_getStorageAt returns a complete 32-byte word, not an optional counter.
    let word: B256 = serde_json::from_value(value.clone())?;
    Ok(U256::from_be_bytes(word.0).try_into()?)
}

pub(in crate::features::ocomp) fn dynamic_deadline_decode_events(
    logs: &serde_json::Value,
    victim: Address,
    jobs: [B256; 2],
    checkpoints: [crate::world::rpc::FinalizedCheckpoint; 2],
) -> eyre::Result<[DynamicDeadlineMiss; 2]> {
    let rows = logs
        .as_array()
        .ok_or_else(|| eyre!("miss events are not an array"))?;
    ensure!(
        rows.len() == 2,
        "exactly two canonical OcompVoteMissed events required"
    );
    let quantity = |value: &serde_json::Value| -> eyre::Result<u64> {
        let word: U256 = serde_json::from_value(value.clone())?;
        Ok(word.try_into()?)
    };
    let mut events = Vec::new();
    for row in rows {
        let source: Address = serde_json::from_value(row["address"].clone())?;
        ensure!(
            source == crate::internal::addresses::WWD_ADDR,
            "wrong OCOMP event emitter"
        );
        ensure!(
            row["removed"].as_bool() == Some(false),
            "removed or unqualified OCOMP event"
        );
        let topics: Vec<B256> = serde_json::from_value(row["topics"].clone())?;
        let data: Bytes = serde_json::from_value(row["data"].clone())?;
        ensure!(
            topics.len() == 3 && data.len() == 128,
            "malformed OcompVoteMissed shape"
        );
        let event = eth::IMetadosis::OcompVoteMissed::decode_raw_log_validate(
            topics.iter().copied(),
            &data,
        )?;
        let canonical = event.encode_log_data();
        ensure!(
            canonical.topics() == topics.as_slice() && canonical.data == data,
            "noncanonical OcompVoteMissed ABI encoding"
        );
        events.push(DynamicDeadlineMiss {
            validator: event.validator,
            job_id: event.jobId,
            miss_count: event.missCount,
            slashed_bonded: event.slashedBonded,
            recovery_deadline: event.recoveryDeadline,
            first_in_window: event.firstInWindow,
            height: quantity(&row["blockNumber"])?,
            block_hash: serde_json::from_value(row["blockHash"].clone())?,
            transaction_hash: serde_json::from_value(row["transactionHash"].clone())?,
            log_index: quantity(&row["logIndex"])?,
        });
    }
    events.sort_by_key(|event| (event.height, event.log_index));
    for (ordinal, event) in events.iter().enumerate() {
        ensure!(
            event.validator == victim && event.job_id == jobs[ordinal],
            "wrong missing validator/job identity"
        );
        ensure!(
            event.height == checkpoints[ordinal].height
                && event.block_hash == checkpoints[ordinal].block_hash,
            "miss event is not in the exact canonical closing block"
        );
    }
    events
        .try_into()
        .map_err(|_| eyre!("two miss events required"))
}

pub(in crate::features::ocomp) fn dynamic_deadline_validate_receipt(
    receipt: &serde_json::Value,
    event: &DynamicDeadlineMiss,
) -> eyre::Result<()> {
    let quantity = |value: &serde_json::Value| -> eyre::Result<u64> {
        let word: U256 = serde_json::from_value(value.clone())?;
        Ok(word.try_into()?)
    };
    let hash = |value: &serde_json::Value| -> eyre::Result<B256> {
        Ok(serde_json::from_value(value.clone())?)
    };
    ensure!(
        quantity(&receipt["status"])? == 1
            && quantity(&receipt["blockNumber"])? == event.height
            && hash(&receipt["blockHash"])? == event.block_hash
            && hash(&receipt["transactionHash"])? == event.transaction_hash
            && event.transaction_hash != B256::ZERO,
        "missing, reverted, or foreign OCOMP system receipt"
    );
    let expected = eth::IMetadosis::OcompVoteMissed {
        validator: event.validator,
        jobId: event.job_id,
        missCount: event.miss_count,
        slashedBonded: event.slashed_bonded,
        recoveryDeadline: event.recovery_deadline,
        firstInWindow: event.first_in_window,
    }
    .encode_log_data();
    let rows = receipt["logs"]
        .as_array()
        .ok_or_else(|| eyre!("receipt has no logs"))?;
    let mut matched = 0;
    for row in rows {
        if quantity(&row["logIndex"])? != event.log_index {
            continue;
        }
        matched += 1;
        let address: Address = serde_json::from_value(row["address"].clone())?;
        let topics: Vec<B256> = serde_json::from_value(row["topics"].clone())?;
        let data: Bytes = serde_json::from_value(row["data"].clone())?;
        ensure!(
            address == crate::internal::addresses::WWD_ADDR
                && topics.as_slice() == expected.topics()
                && data == expected.data
                && row["removed"].as_bool() == Some(false)
                && quantity(&row["blockNumber"])? == event.height
                && hash(&row["blockHash"])? == event.block_hash
                && hash(&row["transactionHash"])? == event.transaction_hash,
            "receipt does not contain the exact canonical OcompVoteMissed log"
        );
    }
    ensure!(matched == 1, "receipt must include the exact miss log once");
    Ok(())
}

pub(in crate::features::ocomp) fn dynamic_deadline_validate_penalties(
    states: &[DynamicDeadlineAccount; 5],
    events: &[DynamicDeadlineMiss; 2],
    members: &[Address],
    deadlines: [u64; 2],
    final_height: u64,
) -> eyre::Result<()> {
    ensure!(members.len() == 5, "five expected identities required");
    let mut sorted_members = members.to_vec();
    sorted_members.sort_unstable();
    ensure!(
        sorted_members.windows(2).all(|pair| pair[0] != pair[1]),
        "duplicate expected validator identity"
    );
    let recovery = deadlines[0]
        .checked_add(DYNAMIC_OCOMP_RECOVERY_BLOCKS)
        .ok_or_else(|| eyre!("recovery deadline overflow"))?;
    ensure!(
        deadlines[0] > 0
            && deadlines[0] < deadlines[1]
            && deadlines[1] < final_height
            && final_height < recovery,
        "observations must pass both deadlines but precede recovery expiry"
    );
    let miss_counts = [0, 1, 1, 2, 2];
    let recovery_deadlines = [0, recovery, recovery, recovery, recovery];
    for (ordinal, state) in states.iter().enumerate() {
        ensure!(
            state.ocomp_miss_count == miss_counts[ordinal]
                && state.ocomp_recovery_deadline == recovery_deadlines[ordinal],
            "durable OCOMP miss count or fixed recovery deadline is incorrect"
        );
        ensure!(
            state.status == 2
                && state.active == sorted_members
                && state.participants == sorted_members,
            "soft penalty must preserve all five ACTIVE consensus participants"
        );
        ensure!(
            state.bonded == state.mirrored,
            "bonded/mirrored stake mismatch"
        );
    }
    let slash = states[0].bonded / U256::from(10);
    ensure!(
        !slash.is_zero(),
        "funded fixture must exercise a positive first-miss slash"
    );
    for (ordinal, event) in events.iter().enumerate() {
        ensure!(
            event.validator == members[3] && event.height == deadlines[ordinal],
            "wrong miss identity/height"
        );
        ensure!(
            event.miss_count == (ordinal + 1) as u64 && event.first_in_window == (ordinal == 0),
            "wrong first/repeat miss count or flag"
        );
        ensure!(
            event.recovery_deadline == recovery,
            "OCOMP recovery deadline moved"
        );
        ensure!(
            event.slashed_bonded == if ordinal == 0 { slash } else { U256::ZERO },
            "OCOMP must slash bonded stake exactly once"
        );
    }
    let mut expected = states[0].clone();
    expected.bonded = expected
        .bonded
        .checked_sub(slash)
        .ok_or_else(|| eyre!("bonded slash underflow"))?;
    expected.mirrored = expected.bonded;
    expected.total_staked = expected
        .total_staked
        .checked_sub(slash)
        .ok_or_else(|| eyre!("total stake underflow"))?;
    expected.staking_balance = expected
        .staking_balance
        .checked_sub(slash)
        .ok_or_else(|| eyre!("staking balance underflow"))?;
    for (ordinal, state) in states.iter().enumerate().skip(1) {
        expected.ocomp_miss_count = miss_counts[ordinal];
        expected.ocomp_recovery_deadline = recovery_deadlines[ordinal];
        ensure!(state == &expected,
            "first/repeat/post-deadline accounting changed beyond one bonded-only slash (including ordinary slash count)");
    }
    Ok(())
}

pub(in crate::features::ocomp) fn dynamic_deadline_validate_accountability(
    closed: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
    baseline: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
    slots: &[u16],
    deadline: u64,
    membership: (u16, u16),
) -> eyre::Result<()> {
    ensure!(
        (closed.member_count, closed.quorum_threshold) == membership
            && (baseline.member_count, baseline.quorum_threshold) == membership,
        "historical membership/quorum changed"
    );
    ensure!(
        closed.job_id == baseline.job_id
            && closed.result_validator_set_epoch == baseline.result_validator_set_epoch
            && closed.result_committee_set_hash == baseline.result_committee_set_hash
            && closed.result_ocomp_binding_hash == baseline.result_ocomp_binding_hash,
        "historical job/binding changed"
    );
    ensure!(
        baseline.quorum_result_digest.is_some()
            && closed.quorum_result_digest == baseline.quorum_result_digest
            && closed.quorum_height == baseline.quorum_height
            && closed.quorum_signer_bitmap == baseline.quorum_signer_bitmap,
        "completed quorum/result changed at close"
    );
    ensure!(
        closed.slot_validator_indexes == slots
            && baseline.slot_validator_indexes == slots
            && closed.slot_first_signatures == baseline.slot_first_signatures,
        "accepted votes changed at close"
    );
    ensure!(
        slots.len() == usize::from(membership.1)
            && slots.windows(2).all(|pair| pair[0] < pair[1])
            && slots.iter().all(|index| *index < membership.0),
        "invalid pinned quorum slots"
    );
    ensure!(
        closed.closed_height == Some(deadline),
        "wrong accountability closing height"
    );
    let missing: Vec<_> = (0..closed.member_count)
        .filter(|index| !slots.contains(index))
        .collect();
    ensure!(
        missing.len() == 1,
        "exactly one historical participant must be missing"
    );
    ensure!(
        closed.missing_bitmap
            == Some(singleton_participant_bitmap(
                closed.member_count,
                missing[0]
            )),
        "wrong missing bitmap"
    );
    Ok(())
}

pub(in crate::features::ocomp) fn assert_job_expires_without_nod(
    world: &mut World,
    expected_voters: &[u16],
    timely_bitmap: u8,
    missing_bitmap: u8,
    expect_export: bool,
    fault_label: &str,
) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized no-quorum JobIntent");
    let primary = world.validators.primary_port();
    wait_for_common_finalized_checkpoint(world, request.deadline_height, fault_label);
    let records = world
        .validators
        .committee_ports()
        .into_iter()
        .map(|port| {
            world
                .rpc
                .ocomp_job_record_at_on(port, request.intent_id, request.deadline_height)
                .unwrap_or_else(|error| {
                    panic!(
                        "read {fault_label} OCOMP record at exact deadline h{} on port {port}: {error:#}",
                        request.deadline_height
                    )
                })
        })
        .collect::<Vec<_>>();
    let record = records[0].clone();
    assert!(
        records.iter().all(|observed| observed == &record),
        "validators expose different expired JobIntent state"
    );
    assert_eq!(
        record.status,
        OcompJobStatus::Expired,
        "{fault_label} OCOMP job was not expired at its exact deadline"
    );
    let terminal = record.terminal.expect("expired terminal record");
    assert_eq!(terminal.outcome, OcompTerminalOutcome::Expired);
    assert_eq!(terminal.terminal_height, request.deadline_height);
    assert!(terminal.completed_binding.is_none());
    let finalized = record.finalized.expect("finalized expired job");
    assert!(finalized.quorum.is_none());

    let accountability = world
        .rpc
        .ocomp_vote_accountability_at_on(primary, finalized.job_id, request.deadline_height)
        .expect("closed no-quorum accountability");
    assert_eq!(accountability.slot_validator_indexes, expected_voters);
    assert_eq!(accountability.quorum_result_digest, None);
    assert_eq!(accountability.closed_height, Some(request.deadline_height));
    assert_eq!(accountability.timely_bitmap, Some(vec![timely_bitmap]));
    assert_eq!(accountability.missing_bitmap, Some(vec![missing_bitmap]));
    assert_eq!(accountability.equivocation_bitmap, Some(vec![0]));

    for port in world.validators.committee_ports() {
        assert_eq!(
            world
                .rpc
                .ocomp_vote_accountability_at_on(port, finalized.job_id, request.deadline_height)
                .expect("exact closed accountability on every validator"),
            accountability
        );
        assert_eq!(
            world.rpc.nod_certified_generation_exists_on(
                port,
                request.worldwide_day,
                request.request_height,
            ),
            Some(false),
            "expired no-quorum job created a Nod generation on port {port}"
        );
        assert!(
            world
                .rpc
                .finalized_ocomp_activation_absent_on(
                    port,
                    request.request_height,
                    request.intent_id,
                )
                .expect("prove absence of finalized Lysis activation"),
            "expired no-quorum job emitted a Lysis apply event on port {port}"
        );
    }
    let retention_blocks = world
        .ocomp
        .canonical_fork_install()
        .expect("read canonical OCOMP retention profile")
        .request_profile
        .capacity_profile
        .source_retention_after_terminal_blocks;
    let terminal_checkpoint = request
        .deadline_height
        .saturating_add(retention_blocks)
        .saturating_add(1);
    wait_for_common_finalized_checkpoint(world, terminal_checkpoint, fault_label);
    for port in world.validators.committee_ports() {
        let record = world
            .rpc
            .ocomp_job_record_at_on(port, request.intent_id, terminal_checkpoint)
            .expect("expired record after terminal finalized checkpoint");
        assert_eq!(record.status, OcompJobStatus::Expired);
        assert_eq!(
            record
                .terminal
                .as_ref()
                .map(|terminal| terminal.terminal_height),
            Some(request.deadline_height)
        );
    }
    assert!(
        terminal_checkpoint
            > world
                .state
                .ocomp_finality_before_fault
                .expect("finality captured before no-quorum OCOMP fault"),
        "terminal checkpoint must advance beyond pre-fault finality"
    );
    wait_for_released_retention(world, finalized.job_id, expect_export, fault_label);
    world.state.ocomp_vote_accountability = Some(accountability);
    world.state.ocomp_expired_without_nod = Some(true);
}

pub(in crate::features::ocomp) fn wait_for_released_retention(
    world: &mut World,
    job_id: B256,
    expect_export: bool,
    fault_label: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut latest = vec![String::new(); 4];
    loop {
        let mut released = 0_usize;
        for (validator_index, observation) in latest.iter_mut().enumerate() {
            let root = retention_journal_root(&world.validators.data_dir(validator_index));
            match inspect_retention_journal(&root) {
                Ok(snapshot) => {
                    let matching = snapshot.records.iter().find_map(|(_, record)| {
                        let PinStateV1::Released {
                            job_id: observed_job_id,
                            source_generation,
                            observed_height,
                            export,
                            ..
                        } = record.state
                        else {
                            return None;
                        };
                        (observed_job_id == job_id).then_some((
                            source_generation,
                            observed_height,
                            export,
                        ))
                    });
                    if let Some((source_generation, observed_height, export)) = matching {
                        assert!(
                            source_generation > 0,
                            "validator-{validator_index} lost the released source generation"
                        );
                        assert_eq!(
                            export.is_some(),
                            expect_export,
                            "validator-{validator_index} released {fault_label} job with unexpected export authority"
                        );
                        if let Some(outage) = &world.state.ocomp_worker_outage {
                            let saved = outage
                                .exports
                                .iter()
                                .find(|item| item.validator_index as usize == validator_index)
                                .expect("saved exact export for each faulted validator");
                            assert_eq!(saved.job_id, job_id);
                            assert_eq!(source_generation, saved.source_generation);
                            assert_eq!(
                                export,
                                Some(outbe_node::ocomp::retention::ExportAuthorityV1 {
                                    source_generation: saved.source_generation,
                                    lease_generation: saved.lease_generation,
                                    manifest_hash: saved.manifest_hash,
                                })
                            );
                            let retention = world
                                .ocomp
                                .canonical_fork_install()
                                .unwrap()
                                .request_profile
                                .capacity_profile
                                .source_retention_after_terminal_blocks;
                            let due = world
                                .state
                                .ocomp_job_request
                                .as_ref()
                                .unwrap()
                                .deadline_height
                                .checked_add(retention)
                                .unwrap();
                            assert!(
                                observed_height >= due,
                                "retention released before its canonical due height"
                            );
                        }
                        *observation = format!(
                            "Released(observed_height={observed_height}, export={})",
                            export.is_some()
                        );
                        released += 1;
                    } else {
                        *observation = format!(
                            "journal_generation={}, records={}, matching_job=absent",
                            snapshot.generation,
                            snapshot.records.len()
                        );
                    }
                }
                Err(error) => {
                    *observation = format!("journal error at {}: {error}", root.display());
                }
            }
        }
        if released == 4 {
            break;
        }
        world
            .localnet
            .ensure_committee_alive()
            .expect("consensus committee remains live while retention GC completes");
        assert!(
            Instant::now() < deadline,
            "{fault_label} job {job_id:#x} did not reach durable Released on every validator: {latest:?}"
        );
        sleep(Duration::from_millis(250));
    }
    world.state.ocomp_expired_release_had_export = Some(expect_export);
}

pub(in crate::features::ocomp) fn retention_journal_root(node_data_dir: &Path) -> PathBuf {
    node_data_dir.join("consensus").join("ocomp_retention")
}
