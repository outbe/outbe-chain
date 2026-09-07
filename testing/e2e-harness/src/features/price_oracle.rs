//! Real price-feeder acceptance steps over a harness-owned HTTP source.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{address, Address, U256};
use cucumber::{given, then};
use eyre::{ensure, eyre, Result};

use crate::internal::{addresses, eth};
use crate::world::localnet::{BootstrapProfile, StartOpts};
use crate::world::price_oracle::{
    CanonicalPublicationObservation, FeederLaunch, OracleCheckpointV1, OracleCohortV1,
    OracleEvidencePhaseV1, OracleMemberV1, PenaltySnapshotEvidenceV1, QuorumLossEvidenceV1,
    QuorumLossPairEvidenceV1, ValidatorPenaltyEvidenceV1,
};
use crate::world::World;

const USD_ISO: u16 = 840;
const MOCK_PRICE: &str = "1.000000";
const MOCK_VOLUME: &str = "1000.000000";
const EXPECTED_RATE: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);
const BTC_TOKEN: Address = address!("2260fac5e5542a773aa44fbcfedf7c193bc2c599");
const FX_TTL_SECS: u64 = 21_600;
const PUBLICATION_TIMEOUT: Duration = Duration::from_secs(180);

#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingPricePublication {
    strictly_after_block: u64,
    expected_rate: U256,
    deadline: Instant,
}

#[given(expr = "a fresh price oracle localnet with a {int}-block voting window")]
fn fresh_price_oracle_localnet(world: &mut World, window: u64) {
    let profile = BootstrapProfile::default()
        .with_oracle_pairs(vec![
            ("COEN".into(), "840".into(), "1000000".into()),
            (format!("{BTC_TOKEN:#x}"), "840".into(), "0".into()),
        ])
        .expect("valid Oracle E2E registry");
    world.state.voting_window = window;
    world.state.wwd = Some(crate::world::localnet::worldwide_day());
    world
        .localnet
        .bootstrap_with_profile(world.validators.size(), &profile)
        .expect("bootstrap price Oracle localnet");
    crate::features::common::start_bootstrapped_localnet(
        world,
        &StartOpts::with_voting_window(window),
    );
}

#[then("independent validator feeders finalize overlapping pair quorums")]
fn independent_feeders_finalize_overlapping_pair_quorums(world: &mut World) {
    assert_eq!(
        world.validators.size(),
        4,
        "scenario requires four validators"
    );
    let usd = outbe_primitives::asset_type::currency_address(USD_ISO);
    let before = world
        .rpc
        .oracle_rate_data(world.validators.primary_port(), USD_ISO)
        .map_or(0, |rate| rate.last_block);
    let before_b = world
        .rpc
        .oracle_rate_data_for_pair(world.validators.primary_port(), BTC_TOKEN, usd)
        .map_or(0, |rate| rate.last_block);
    let vote_period = start_overlapping_feeders(world);
    let cohort_height = world
        .price_oracle
        .cohort()
        .expect("initial overlapping Oracle cohort")
        .checkpoint
        .height;
    let before = before.max(cohort_height);
    let before_b = before_b.max(cohort_height);

    wait_for_unanimous_pair_publication(
        world,
        OracleEvidencePhaseV1::Initial,
        Address::ZERO,
        usd,
        before,
        EXPECTED_RATE,
        true,
    );
    wait_for_unanimous_pair_publication(
        world,
        OracleEvidencePhaseV1::Initial,
        BTC_TOKEN,
        usd,
        before_b,
        U256::from(133_333_333_333_333_333_333u128),
        true,
    );

    let expected_volume = U256::from(60u64) * outbe_primitives::units::SCALE_1E18;
    for port in world.validators.committee_ports() {
        assert_eq!(
            world.rpc.oracle_latest_volume(port, BTC_TOKEN, usd),
            Some(expected_volume),
            "target volume must use the full raw 3-validator ballot"
        );
    }

    let penalties = (0..4)
        .map(|index| {
            let key = world.validators.get(index).evm_key().unwrap();
            let validator = world
                .rpc
                .address_of(&key)
                .unwrap()
                .parse::<Address>()
                .unwrap();
            let (success, abstain, miss) = world
                .rpc
                .oracle_penalty_counts(world.validators.primary_port(), validator)
                .unwrap();
            ValidatorPenaltyEvidenceV1 {
                validator_index: index,
                validator_address: format!("{validator:#x}"),
                success,
                miss,
                abstain,
            }
        })
        .collect::<Vec<_>>();
    assert!(penalties[0].success == 0 && penalties[0].miss > 0);
    assert!(penalties[1].success > 0 && penalties[1].miss == 0);
    assert!(penalties[2].success > 0 && penalties[2].miss == 0);
    assert!(penalties[3].success == 0 && penalties[3].miss > 0);
    world
        .price_oracle
        .record_penalty_snapshot(PenaltySnapshotEvidenceV1 {
            phase: OracleEvidencePhaseV1::Initial,
            validators: penalties,
        });

    let evidence = world.price_oracle.evidence_snapshot();
    assert!(evidence.ticker_requests > 0);
    assert!(evidence.candle_requests > 0);
    assert_eq!(evidence.feeder_processes.len(), 4);
    let mut distinct_pids = evidence
        .feeder_processes
        .iter()
        .map(|process| process.pid)
        .collect::<Vec<_>>();
    distinct_pids.sort_unstable();
    distinct_pids.dedup();
    assert_eq!(distinct_pids.len(), 4);
    assert_eq!(evidence.feeder_processes[0].oracle_pairs.len(), 1);
    assert_eq!(evidence.feeder_processes[1].oracle_pairs.len(), 2);
    assert_eq!(evidence.feeder_processes[2].oracle_pairs.len(), 2);
    assert_eq!(evidence.feeder_processes[3].oracle_pairs.len(), 1);

    world
        .price_oracle
        .stop_validator_feeder(2)
        .expect("stop one quorum feeder");
    let checkpoint_height = wait_finalized_blocks(world, vote_period * 2);
    let checkpoint_a = world
        .rpc
        .oracle_rate_data(world.validators.primary_port(), USD_ISO)
        .unwrap();
    let checkpoint_b = world
        .rpc
        .oracle_rate_data_for_pair(world.validators.primary_port(), BTC_TOKEN, usd)
        .unwrap();
    wait_until_finalized_height(world, checkpoint_height + vote_period + 2);
    for port in world.validators.committee_ports() {
        assert_eq!(
            world
                .rpc
                .oracle_rate_data(port, USD_ISO)
                .unwrap()
                .last_block,
            checkpoint_a.last_block
        );
        assert_eq!(
            world
                .rpc
                .oracle_rate_data_for_pair(port, BTC_TOKEN, usd)
                .unwrap()
                .last_block,
            checkpoint_b.last_block
        );
    }
    world.price_oracle.record_quorum_loss(QuorumLossEvidenceV1 {
        stopped_validator_index: 2,
        finalized_height_before: checkpoint_height,
        finalized_height_after: checkpoint_height + vote_period + 2,
        pairs: vec![
            QuorumLossPairEvidenceV1 {
                base: format!("{:#x}", Address::ZERO),
                quote: format!("{usd:#x}"),
                last_block_before: checkpoint_a.last_block,
                last_block_after: checkpoint_a.last_block,
            },
            QuorumLossPairEvidenceV1 {
                base: format!("{BTC_TOKEN:#x}"),
                quote: format!("{usd:#x}"),
                last_block_before: checkpoint_b.last_block,
                last_block_after: checkpoint_b.last_block,
            },
        ],
    });

    start_overlap_feeder(world, 2, vote_period, OracleEvidencePhaseV1::QuorumRecovery);
    wait_for_unanimous_pair_publication(
        world,
        OracleEvidencePhaseV1::QuorumRecovery,
        Address::ZERO,
        usd,
        checkpoint_a.last_block,
        EXPECTED_RATE,
        true,
    );
    wait_for_unanimous_pair_publication(
        world,
        OracleEvidencePhaseV1::QuorumRecovery,
        BTC_TOKEN,
        usd,
        checkpoint_b.last_block,
        U256::from(133_333_333_333_333_333_333u128),
        true,
    );
}

#[then("the controlled COEN USD quote is finalized through the real price feeder")]
fn controlled_quote_is_finalized(world: &mut World) {
    start_feeder(world, OracleEvidencePhaseV1::Initial);
    assert_controlled_quote_is_finalized(world);
}

#[then("all five admitted validators finalize the controlled COEN USD quote through the real price feeder")]
fn five_admitted_validators_finalize_controlled_quote(world: &mut World) {
    assert_eq!(world.validators.size(), 4, "four founders plus one joiner");
    let expected_indices = (0..5).collect::<Vec<_>>();
    let expected = expected_indices
        .iter()
        .map(|&index| {
            let key = world
                .validators
                .get(index)
                .evm_key()
                .expect("Oracle admission identity");
            eth::address_of(&key).expect("derive public Oracle admission identity")
        })
        .collect::<Vec<_>>();
    let deadline = Instant::now() + PUBLICATION_TIMEOUT;
    let cohort = loop {
        assert_eq!(
            world.localnet.owned_validator_indices(),
            expected_indices,
            "five exact owned Oracle admission observers required"
        );
        // Resolution checks every owned node/enclave and reads ACTIVE at one
        // common finalized hash/root, never the caller's primary latest state.
        let cohort = resolve_cohort(world, false).expect("finalized Oracle admission cohort");
        if validate_membership(&cohort, &expected).is_ok() {
            break cohort;
        }
        assert!(
            Instant::now() < deadline,
            "all five admitted identities did not become finalized ACTIVE; h{} has {} members",
            cohort.checkpoint.height,
            cohort.members.len()
        );
        sleep(Duration::from_millis(250));
    };
    // Keep the exact admitted cohort: a second generic resolution could select
    // a different membership boundary instead of the one just established.
    start_feeder_with_cohort(world, OracleEvidencePhaseV1::Initial, cohort);
    assert_controlled_quote_is_finalized(world);
}

fn assert_controlled_quote_is_finalized(world: &mut World) {
    let cohort = world.price_oracle.cohort().expect("initial Oracle cohort");
    wait_for_unanimous_publication(
        world,
        OracleEvidencePhaseV1::Initial,
        cohort.checkpoint.height,
        EXPECTED_RATE,
        true,
    );
    let evidence = world.price_oracle.evidence_snapshot();
    assert!(evidence.ticker_requests > 0);
    assert!(evidence.candle_requests > 0);
    let quorum = cohort.quorum;
    let current = evidence
        .feeder_processes
        .iter()
        .filter(|process| process.stopped_at_millis.is_none())
        .collect::<Vec<_>>();
    assert_eq!(current.len(), quorum);
    let mut distinct_pids = evidence
        .feeder_processes
        .iter()
        .filter(|process| process.stopped_at_millis.is_none())
        .map(|process| process.pid)
        .collect::<Vec<_>>();
    distinct_pids.sort_unstable();
    distinct_pids.dedup();
    assert_eq!(distinct_pids.len(), quorum);
}

/// Stop the feeder before a controlled-time restart and retain the last
/// finalized publication as the strict post-restart lower bound. Returning
/// `None` keeps scenarios without the feeder unchanged.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn stop_before_clock_restart(world: &mut World) -> Option<u64> {
    let cohort = match world.price_oracle.cohort() {
        Ok(cohort) => cohort,
        Err(_) if !world.price_oracle.is_feeder_running() => return None,
        Err(error) => panic!("running Oracle feeders lack a cohort: {error:#}"),
    };
    world
        .price_oracle
        .ensure_cohort_feeders_alive()
        .expect("price feeder stays live before clock restart");
    let checkpoint =
        cohort_checkpoint(world, &cohort).expect("pre-restart finalized Oracle barrier");
    let previous =
        publication_lower_bound(world.price_oracle.last_oracle_block(), checkpoint.height);
    world.price_oracle.record_cohort_observation(serde_json::json!({
        "kind": "clock_restart_anchor", "checkpoint": checkpoint, "strictly_after_block": previous,
    }));
    world.price_oracle.stop_feeder();
    Some(previous)
}

/// Restart the feeder with reset poll/backoff state and return the strict
/// publication condition that the caller must observe alongside WWD progress.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn resume_after_clock_restart(
    world: &mut World,
    previous_block: Option<u64>,
) -> Option<PendingPricePublication> {
    let previous_block = previous_block?;
    let expected_rate = feeder_restart_expected_rate(world.price_oracle.read_controlled_quote());
    start_feeder(world, OracleEvidencePhaseV1::ClockRestart);
    let cohort = world
        .price_oracle
        .cohort()
        .expect("restarted Oracle cohort");
    Some(PendingPricePublication {
        strictly_after_block: publication_lower_bound(
            Some(previous_block),
            cohort.checkpoint.height,
        ),
        expected_rate,
        deadline: Instant::now() + PUBLICATION_TIMEOUT,
    })
}

/// Poll one post-restart publication without hiding an irreversible lifecycle
/// transition behind a nested wait. The caller owns the joint WWD/Oracle
/// barrier and stops polling after the first successful observation.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn observe_pending_publication(
    world: &mut World,
    pending: &PendingPricePublication,
) -> bool {
    if observe_unanimous_publication(
        world,
        OracleEvidencePhaseV1::ClockRestart,
        pending.strictly_after_block,
        pending.expected_rate,
        true,
    ) {
        return true;
    }
    assert!(
        Instant::now() < pending.deadline,
        "controlled quote did not publish a newer unanimous finalized Oracle rate within {PUBLICATION_TIMEOUT:?} after block {}",
        pending.strictly_after_block,
    );
    false
}

/// Atomically change the harness-owned quote and prove that the production
/// feeder published the new exact scale-6 rate on every validator.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn publish_controlled_quote(world: &mut World, expected_rate: U256) {
    let cohort = world
        .price_oracle
        .cohort()
        .expect("controlled update Oracle cohort");
    let checkpoint =
        cohort_checkpoint(world, &cohort).expect("controlled update finalized barrier");
    let strictly_after_block =
        publication_lower_bound(world.price_oracle.last_oracle_block(), checkpoint.height);
    world
        .price_oracle
        .record_cohort_observation(serde_json::json!({
            "kind": "controlled_update_anchor", "checkpoint": checkpoint,
            "strictly_after_block": strictly_after_block,
        }));
    let quote = scale6_quote(expected_rate);
    world
        .price_oracle
        .publish_quote(OracleEvidencePhaseV1::ControlledUpdate, &quote, MOCK_VOLUME)
        .expect("publish controlled Oracle quote generation");
    wait_for_unanimous_publication(
        world,
        OracleEvidencePhaseV1::ControlledUpdate,
        strictly_after_block,
        expected_rate,
        true,
    );
}

fn start_feeder(world: &mut World, phase: OracleEvidencePhaseV1) {
    let cohort = resolve_cohort(world, false).expect("resolve finalized ACTIVE Oracle cohort");
    start_feeder_with_cohort(world, phase, cohort);
}

fn start_feeder_with_cohort(
    world: &mut World,
    phase: OracleEvidencePhaseV1,
    cohort: OracleCohortV1,
) {
    let (price, volume) = feeder_start_quote(world.price_oracle.read_controlled_quote());
    world.price_oracle.install_cohort(phase, cohort.clone());
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("read feeder chain id");
    let vote_period =
        cohort_vote_period(world, &cohort).expect("canonical cohort Oracle vote period");

    let feeders = cohort
        .feeder_indices
        .iter()
        .copied()
        .map(|validator_index| {
            let validator = world.validators.get(validator_index);
            let private_key = validator.evm_key().unwrap_or_else(|error| {
                panic!("validator-{validator_index} EVM key for feeder: {error:#}")
            });
            let validator_address = world
                .rpc
                .address_of(&private_key)
                .unwrap_or_else(|| panic!("derive validator-{validator_index} feeder address"));
            assert_eq!(
                validator_address
                    .parse::<Address>()
                    .expect("public feeder address"),
                cohort
                    .members
                    .iter()
                    .find(|member| member.index == validator_index)
                    .expect("selected Oracle owner")
                    .address,
                "feeder identity changed after finalized cohort resolution"
            );
            let rpc_url = world.rpc.url(world.validators.http_port(validator_index));
            (validator_index, rpc_url, private_key, validator_address)
        })
        .collect::<Vec<_>>();

    for (validator_index, rpc_url, private_key, validator_address) in feeders {
        world
            .price_oracle
            .start(
                FeederLaunch {
                    validator_index,
                    rpc_url: &rpc_url,
                    chain_id,
                    private_key: &private_key,
                    validator_address: &validator_address,
                    vote_period,
                    phase,
                },
                crate::world::price_oracle::PriceQuote {
                    price: &price,
                    volume: &volume,
                },
            )
            .unwrap_or_else(|error| {
                panic!("start validator-{validator_index} production price feeder: {error:#}")
            });
    }
}

fn start_overlapping_feeders(world: &mut World) -> u64 {
    let cohort =
        resolve_cohort(world, true).expect("fixed four-validator overlapping Oracle cohort");
    let vote_period = cohort_vote_period(world, &cohort).expect("fixed cohort Oracle vote period");
    world
        .price_oracle
        .install_cohort(OracleEvidencePhaseV1::Initial, cohort);
    for validator_index in 0..4 {
        start_overlap_feeder(
            world,
            validator_index,
            vote_period,
            OracleEvidencePhaseV1::Initial,
        );
    }
    vote_period
}

fn start_overlap_feeder(
    world: &mut World,
    validator_index: usize,
    vote_period: u64,
    phase: OracleEvidencePhaseV1,
) {
    let validator = world.validators.get(validator_index);
    let private_key = validator.evm_key().unwrap_or_else(|error| {
        panic!("validator-{validator_index} EVM key for feeder: {error:#}")
    });
    let validator_address = world
        .rpc
        .address_of(&private_key)
        .unwrap_or_else(|| panic!("derive validator-{validator_index} feeder address"));
    let rpc_url = world.rpc.url(world.validators.http_port(validator_index));
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("read feeder chain id");
    let pairs = overlap_pairs(validator_index);
    world
        .price_oracle
        .start_with_pairs(
            FeederLaunch {
                validator_index,
                rpc_url: &rpc_url,
                chain_id,
                private_key: &private_key,
                validator_address: &validator_address,
                vote_period,
                phase,
            },
            &pairs,
        )
        .unwrap_or_else(|error| {
            panic!("start validator-{validator_index} production price feeder: {error:#}")
        });
}

fn overlap_pairs(validator_index: usize) -> Vec<crate::world::price_oracle::FeederPair<'static>> {
    use crate::world::price_oracle::{FeederPair, FeederSource};

    let pair_a = || FeederPair {
        base: "COEN",
        quote: "840",
        sources: vec![
            FeederSource {
                base: "COEN",
                quote: "USDT",
                price: "1",
                volume: "5",
            },
            FeederSource {
                base: "COEN",
                quote: "USDC",
                price: "1",
                volume: "5",
            },
        ],
    };
    let pair_b = |price, volume| FeederPair {
        base: "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599",
        quote: "840",
        sources: vec![FeederSource {
            base: "BTC",
            quote: "USDT",
            price,
            volume,
        }],
    };

    match validator_index {
        0 => vec![pair_a()],
        1 => vec![pair_a(), pair_b("100", "10")],
        2 => vec![pair_a(), pair_b("200", "20")],
        3 => vec![pair_b("300", "30")],
        _ => panic!("overlap fixture has exactly four validators"),
    }
}

fn oracle_quorum(active_validators: usize) -> usize {
    active_validators - active_validators / 3
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn publication_lower_bound(last_publication: Option<u64>, barrier: u64) -> u64 {
    last_publication.unwrap_or(0).max(barrier)
}

fn plan_cohort(
    checkpoint: OracleCheckpointV1,
    active: &[Address],
    owned: &[OracleMemberV1],
    overlapping_pairs: bool,
) -> Result<OracleCohortV1> {
    use std::collections::BTreeSet;
    ensure!(!active.is_empty(), "Oracle ACTIVE membership is empty");
    let active_set = active.iter().copied().collect::<BTreeSet<_>>();
    ensure!(
        active_set.len() == active.len() && !active_set.contains(&Address::ZERO),
        "invalid ACTIVE identities"
    );
    for unique in [
        owned
            .iter()
            .map(|member| member.index)
            .collect::<BTreeSet<_>>()
            .len(),
        owned
            .iter()
            .map(|member| u64::from(member.port))
            .collect::<BTreeSet<_>>()
            .len(),
        owned
            .iter()
            .map(|member| u64::from(member.node_pid))
            .collect::<BTreeSet<_>>()
            .len(),
        owned
            .iter()
            .map(|member| member.address)
            .collect::<BTreeSet<_>>()
            .len(),
    ] {
        ensure!(
            unique == owned.len(),
            "duplicate Oracle owner identity, index, port or PID"
        );
    }
    ensure!(
        owned
            .iter()
            .all(|member| member.node_pid > 0 && member.port > 0),
        "invalid owned Oracle endpoint"
    );
    let mut members = owned
        .iter()
        .filter(|member| active_set.contains(&member.address))
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        members.len() == active.len(),
        "ACTIVE Oracle validator has no known owned identity"
    );
    members.sort_by_key(|member| member.index);
    let quorum = oracle_quorum(members.len());
    if overlapping_pairs {
        ensure!(
            members.iter().map(|member| member.index).eq(0..4),
            "overlapping-pair negative fixture requires exactly founders 0..4 ACTIVE"
        );
    }
    let feeder_indices = members
        .iter()
        .take(if overlapping_pairs {
            members.len()
        } else {
            quorum
        })
        .map(|member| member.index)
        .collect();
    Ok(OracleCohortV1 {
        checkpoint,
        members,
        quorum,
        feeder_indices,
        overlapping_pairs,
    })
}

fn active_at(world: &World, port: u16, height: u64) -> Result<Vec<Address>> {
    eth::read_call_at_result(
        &world.rpc.url(port),
        addresses::VS_ADDR,
        &eth::IValidatorSet::getActiveValidatorsCall {},
        height,
    )
    .map_err(|error| eyre!(error))
}

fn resolve_cohort(world: &mut World, overlapping_pairs: bool) -> Result<OracleCohortV1> {
    let indices = world.localnet.owned_validator_indices();
    ensure!(
        (0..world.validators.size()).all(|index| indices.contains(&index)),
        "original Oracle observer owner missing"
    );
    let mut owned = Vec::with_capacity(indices.len());
    for index in indices {
        let (node_pid, _) = world.localnet.live_validator_and_enclave_pids(index)?;
        let key = world.validators.get(index).evm_key()?;
        let address =
            eth::address_of(&key).ok_or_else(|| eyre!("derive public Oracle owner {index}"))?;
        owned.push(OracleMemberV1 {
            index,
            address,
            port: world.validators.http_port(index),
            node_pid,
        });
    }
    let ports = owned.iter().map(|member| member.port).collect::<Vec<_>>();
    let checkpoint = world.rpc.wait_finalized_checkpoint(&ports, 0, 1)?;
    let active = active_at(world, ports[0], checkpoint.height)?;
    for &port in &ports {
        ensure!(
            active_at(world, port, checkpoint.height)? == active,
            "Oracle ACTIVE membership disagreement at h{}",
            checkpoint.height
        );
    }
    plan_cohort(
        OracleCheckpointV1 {
            height: checkpoint.height,
            block_hash: checkpoint.block_hash,
            state_root: checkpoint.state_root,
        },
        &active,
        &owned,
        overlapping_pairs,
    )
}

fn validate_membership(cohort: &OracleCohortV1, active: &[Address]) -> Result<()> {
    let expected = cohort
        .members
        .iter()
        .map(|member| member.address)
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        active.len() == expected.len()
            && active
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                == expected,
        "Oracle ACTIVE membership changed during publication"
    );
    Ok(())
}

fn cohort_checkpoint(world: &mut World, cohort: &OracleCohortV1) -> Result<OracleCheckpointV1> {
    let ports = cohort
        .members
        .iter()
        .map(|member| member.port)
        .collect::<Vec<_>>();
    for member in &cohort.members {
        let (pid, _) = world
            .localnet
            .live_validator_and_enclave_pids(member.index)?;
        ensure!(
            pid == member.node_pid,
            "Oracle node incarnation changed without a new cohort"
        );
    }
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, cohort.checkpoint.height, 1)?;
    for &port in &ports {
        validate_membership(cohort, &active_at(world, port, checkpoint.height)?)?;
    }
    Ok(OracleCheckpointV1 {
        height: checkpoint.height,
        block_hash: checkpoint.block_hash,
        state_root: checkpoint.state_root,
    })
}

fn cohort_vote_period(world: &World, cohort: &OracleCohortV1) -> Result<u64> {
    let mut expected = None;
    for member in &cohort.members {
        let params = eth::read_call_at_result(
            &world.rpc.url(member.port),
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &eth::IOracle::getParamsCall {},
            cohort.checkpoint.height,
        )
        .map_err(|error| eyre!(error))?;
        ensure!(
            params.votePeriod > 0 && expected.is_none_or(|period| period == params.votePeriod),
            "Oracle vote period mismatch"
        );
        expected = Some(params.votePeriod);
    }
    expected.ok_or_else(|| eyre!("no Oracle vote-period observers"))
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct PublicationRead {
    port: u16,
    finalized: u64,
    checkpoint: OracleCheckpointV1,
    rate: U256,
    volume: Option<U256>,
    oracle_block: u64,
    oracle_timestamp: u64,
    finalized_timestamp: u64,
}

fn evaluate_publication(
    ports: &[u16],
    reads: &[PublicationRead],
    after: u64,
    expected_rate: U256,
) -> Result<bool> {
    ensure!(
        !ports.is_empty()
            && ports
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == ports.len(),
        "invalid expected Oracle peers"
    );
    ensure!(
        reads.iter().map(|read| read.port).eq(ports.iter().copied()),
        "Oracle peer omitted, duplicated or unexpected"
    );
    let first = &reads[0];
    for read in reads {
        ensure!(
            read.finalized >= first.checkpoint.height && read.checkpoint == first.checkpoint,
            "Oracle read is not at a common finalized hash/root"
        );
        ensure!(
            read.rate == first.rate
                && read.volume == first.volume
                && read.oracle_block == first.oracle_block
                && read.oracle_timestamp == first.oracle_timestamp
                && read.finalized_timestamp == first.finalized_timestamp,
            "Oracle pinned state disagreement"
        );
    }
    ensure!(
        first.oracle_block <= first.checkpoint.height
            && first.oracle_timestamp <= first.finalized_timestamp,
        "Oracle publication is ahead of finalized observation"
    );
    Ok(first.volume.is_some()
        && first.rate == expected_rate
        && first.oracle_block > after
        && first.oracle_timestamp > 0
        && first.finalized_timestamp - first.oracle_timestamp <= FX_TTL_SECS)
}

fn feeder_start_quote(current: Option<(String, String)>) -> (String, String) {
    current.unwrap_or_else(|| (MOCK_PRICE.to_owned(), MOCK_VOLUME.to_owned()))
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn feeder_restart_expected_rate(current: Option<(String, String)>) -> U256 {
    let (price, _) = feeder_start_quote(current);
    parse_scale6_rate(&price).unwrap_or_else(|| {
        panic!("controlled feeder quote `{price}` is not a canonical scale-6 rate")
    })
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn parse_scale6_rate(price: &str) -> Option<U256> {
    let (whole, fraction) = price.split_once('.').unwrap_or((price, ""));
    if whole.is_empty()
        || fraction.len() > 6
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let whole = whole.parse::<U256>().ok()?;
    let mut fraction = fraction.to_owned();
    fraction.extend(std::iter::repeat_n('0', 6 - fraction.len()));
    let fraction = if fraction.is_empty() {
        U256::ZERO
    } else {
        fraction.parse::<U256>().ok()?
    };
    whole
        .checked_mul(U256::from(1_000_000_u64))?
        .checked_add(fraction)
}

fn wait_for_unanimous_publication(
    world: &mut World,
    phase: OracleEvidencePhaseV1,
    strictly_after_block: u64,
    expected_rate: U256,
    require_live_feeder: bool,
) {
    wait_for_unanimous_pair_publication(
        world,
        phase,
        Address::ZERO,
        outbe_primitives::asset_type::currency_address(USD_ISO),
        strictly_after_block,
        expected_rate,
        require_live_feeder,
    );
}

fn wait_for_unanimous_pair_publication(
    world: &mut World,
    phase: OracleEvidencePhaseV1,
    base: Address,
    quote: Address,
    strictly_after_block: u64,
    expected_rate: U256,
    require_live_feeder: bool,
) {
    let deadline = Instant::now() + PUBLICATION_TIMEOUT;
    loop {
        if observe_unanimous_pair_publication(
            world,
            phase,
            base,
            quote,
            strictly_after_block,
            expected_rate,
            require_live_feeder,
        ) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "controlled quote did not become one fresh finalized Oracle publication: expected_rate={expected_rate} after_block={strictly_after_block}"
        );
        sleep(Duration::from_millis(250));
    }
}

#[cfg(feature = "ocomp-integration")]
fn observe_unanimous_publication(
    world: &mut World,
    phase: OracleEvidencePhaseV1,
    strictly_after_block: u64,
    expected_rate: U256,
    require_live_feeder: bool,
) -> bool {
    observe_unanimous_pair_publication(
        world,
        phase,
        Address::ZERO,
        outbe_primitives::asset_type::currency_address(USD_ISO),
        strictly_after_block,
        expected_rate,
        require_live_feeder,
    )
}

fn observe_unanimous_pair_publication(
    world: &mut World,
    phase: OracleEvidencePhaseV1,
    base: Address,
    quote: Address,
    strictly_after_block: u64,
    expected_rate: U256,
    require_live_feeder: bool,
) -> bool {
    let result = observe_cohort_publication(
        world,
        phase,
        base,
        quote,
        strictly_after_block,
        expected_rate,
        require_live_feeder,
    );
    if let Err(error) = &result {
        world
            .price_oracle
            .record_cohort_observation(serde_json::json!({
                "kind": "publication_error", "error": format!("{error:#}"),
            }));
    }
    result.expect("observe complete finalized Oracle cohort")
}

fn observe_cohort_publication(
    world: &mut World,
    phase: OracleEvidencePhaseV1,
    base: Address,
    quote: Address,
    strictly_after_block: u64,
    expected_rate: U256,
    require_live_feeder: bool,
) -> Result<bool> {
    if require_live_feeder {
        world.price_oracle.ensure_cohort_feeders_alive()?;
    }
    let cohort = world.price_oracle.cohort()?;
    let checkpoint = cohort_checkpoint(world, &cohort)?;
    let ports = cohort
        .members
        .iter()
        .map(|member| member.port)
        .collect::<Vec<_>>();
    let mut reads = Vec::with_capacity(ports.len());
    for &port in &ports {
        let url = world.rpc.url(port);
        let rate = eth::read_call_at_result(
            &url,
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &eth::IOracle::getExchangeRateDataCall { base, quote },
            checkpoint.height,
        )
        .map_err(|error| eyre!(error))?;
        let history = eth::read_call_at_result(
            &url,
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &eth::IOracle::getPriceSnapshotHistoryCall {
                base,
                quote,
                count: 1,
            },
            checkpoint.height,
        )
        .map_err(|error| eyre!(error))?;
        let volume = history.volumes.first().copied();
        let finalized_timestamp = world
            .rpc
            .block_timestamp(port, checkpoint.height)
            .ok_or_else(|| eyre!("Oracle finalized timestamp unavailable on {port}"))?;
        let canonical = world.rpc.checkpoint_at(port, checkpoint.height)?;
        ensure!(
            canonical.block_hash == checkpoint.block_hash
                && canonical.state_root == checkpoint.state_root,
            "Oracle checkpoint changed during pinned read"
        );
        reads.push(PublicationRead {
            port,
            finalized: world.rpc.finalized_result(port)?,
            checkpoint: OracleCheckpointV1 {
                height: canonical.height,
                block_hash: canonical.block_hash,
                state_root: canonical.state_root,
            },
            rate: rate.rate,
            volume,
            oracle_block: rate.lastBlock,
            oracle_timestamp: rate.lastTimestamp,
            finalized_timestamp,
        });
        world
            .price_oracle
            .record_cohort_observation(serde_json::json!({
                "kind": "publication_peer", "phase": phase, "base": base, "quote": quote,
                "strictly_after_block": strictly_after_block, "expected_rate": expected_rate,
                "read": reads.last(),
            }));
    }
    if !evaluate_publication(&ports, &reads, strictly_after_block, expected_rate)? {
        return Ok(false);
    }
    let first = &reads[0];
    for &port in &ports {
        validate_membership(&cohort, &active_at(world, port, first.oracle_block)?)?;
    }
    world
        .price_oracle
        .record_canonical_publication(CanonicalPublicationObservation {
            phase,
            validator_count: ports.len(),
            base,
            quote,
            rate: first.rate,
            volume: first.volume.expect("publication evaluator required volume"),
            oracle_block: first.oracle_block,
            oracle_timestamp: first.oracle_timestamp,
            finalized_height: checkpoint.height,
            finalized_timestamp: first.finalized_timestamp,
        });
    world
        .price_oracle
        .record_cohort_observation(serde_json::json!({
            "kind": "publication_verified", "phase": phase, "base": base, "quote": quote,
            "strictly_after_block": strictly_after_block, "checkpoint": checkpoint,
            "oracle_block": first.oracle_block,
            "reads": reads,
        }));
    Ok(true)
}

fn wait_finalized_blocks(world: &mut World, blocks: u64) -> u64 {
    let current = world
        .validators
        .committee_ports()
        .iter()
        .filter_map(|port| world.rpc.finalized(*port))
        .min()
        .expect("read committee finality before quorum-loss window");
    let target = current.saturating_add(blocks);
    wait_until_finalized_height(world, target);
    target
}

fn wait_until_finalized_height(world: &mut World, target: u64) {
    let deadline = Instant::now() + PUBLICATION_TIMEOUT;
    loop {
        let ports = world.validators.committee_ports();
        if ports.iter().all(|port| {
            world
                .rpc
                .finalized(*port)
                .is_some_and(|height| height >= target)
        }) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "committee did not finalize height {target} during Oracle quorum-loss window"
        );
        sleep(Duration::from_millis(250));
    }
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn scale6_quote(rate: U256) -> String {
    let scale = U256::from(1_000_000_u64);
    let whole = rate / scale;
    let fraction = rate % scale;
    format!("{whole}.{fraction:06}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint() -> OracleCheckpointV1 {
        OracleCheckpointV1 {
            height: 20,
            block_hash: alloy_primitives::B256::repeat_byte(1),
            state_root: alloy_primitives::B256::repeat_byte(2),
        }
    }

    fn owned(indices: &[usize]) -> Vec<OracleMemberV1> {
        indices
            .iter()
            .map(|&index| OracleMemberV1 {
                index,
                address: Address::repeat_byte(u8::try_from(index + 1).unwrap()),
                port: 8000 + u16::try_from(index).unwrap(),
                node_pid: 100 + u32::try_from(index).unwrap(),
            })
            .collect()
    }

    fn reads() -> Vec<PublicationRead> {
        [8000, 8001, 8002, 8003, 8004]
            .into_iter()
            .map(|port| PublicationRead {
                port,
                finalized: 22,
                checkpoint: checkpoint(),
                rate: EXPECTED_RATE,
                volume: Some(U256::from(100)),
                oracle_block: 19,
                oracle_timestamp: 100,
                finalized_timestamp: 110,
            })
            .collect()
    }

    #[test]
    fn cohort_uses_active_identity_not_bootstrap_size_or_dense_indices() {
        let all = owned(&[0, 1, 2, 3, 4]);
        let active = all.iter().map(|member| member.address).collect::<Vec<_>>();
        let four = plan_cohort(checkpoint(), &active[..4], &all, false).unwrap();
        assert_eq!(four.feeder_indices, [0, 1, 2]);
        assert_eq!(four.members.len(), 4); // owned but non-ACTIVE joiner excluded
        let five = plan_cohort(checkpoint(), &active, &all, false).unwrap();
        assert_eq!(five.feeder_indices, [0, 1, 2, 3]);
        assert_eq!(five.members.len(), 5);
        assert_eq!(five.quorum, 4);
        let sparse = owned(&[0, 2, 4, 7, 9]);
        let active = sparse
            .iter()
            .map(|member| member.address)
            .collect::<Vec<_>>();
        assert_eq!(
            plan_cohort(checkpoint(), &active, &sparse, false)
                .unwrap()
                .feeder_indices,
            [0, 2, 4, 7]
        );
    }

    #[test]
    fn admission_handoff_waits_for_five_finalized_identities_without_changing_generic_planning() {
        let all = owned(&[0, 1, 2, 3, 4]);
        let expected = all.iter().map(|member| member.address).collect::<Vec<_>>();
        // Primary latest may already report five, but the common finalized
        // checkpoint still has only four. It cannot authorize admission launch.
        let before = plan_cohort(checkpoint(), &expected[..4], &all, false).unwrap();
        assert!(validate_membership(&before, &expected).is_err());
        // Generic callers still legitimately exclude an owned PENDING joiner.
        assert_eq!(before.members.len(), 4);
        assert_eq!(before.feeder_indices, [0, 1, 2]);

        let admitted_checkpoint = OracleCheckpointV1 {
            height: 21,
            block_hash: alloy_primitives::B256::repeat_byte(3),
            state_root: alloy_primitives::B256::repeat_byte(4),
        };
        let admitted = plan_cohort(admitted_checkpoint.clone(), &expected, &all, false).unwrap();
        validate_membership(&admitted, &expected).unwrap();
        assert_eq!(admitted.checkpoint, admitted_checkpoint);
        assert_eq!(admitted.members, all);
        assert_eq!(admitted.quorum, 4);
        assert_eq!(admitted.feeder_indices, [0, 1, 2, 3]);
        let mut wrong_identity = expected;
        wrong_identity[4] = Address::repeat_byte(99);
        assert!(validate_membership(&admitted, &wrong_identity).is_err());
    }

    #[test]
    fn cohort_rejects_unmapped_duplicate_empty_and_changed_membership() {
        let all = owned(&[0, 1, 2, 3, 4]);
        let active = all.iter().map(|member| member.address).collect::<Vec<_>>();
        assert!(plan_cohort(checkpoint(), &[], &all, false).is_err());
        assert!(plan_cohort(checkpoint(), &active, &all[..4], false).is_err());
        assert!(plan_cohort(checkpoint(), &[active[0], active[0]], &all, false).is_err());
        for field in 0..4 {
            let mut wrong = all.clone();
            match field {
                0 => wrong[4].address = wrong[0].address,
                1 => wrong[4].index = wrong[0].index,
                2 => wrong[4].port = wrong[0].port,
                _ => wrong[4].node_pid = wrong[0].node_pid,
            }
            assert!(plan_cohort(checkpoint(), &active, &wrong, false).is_err());
        }
        let cohort = plan_cohort(checkpoint(), &active, &all, false).unwrap();
        assert!(validate_membership(&cohort, &active).is_ok());
        assert!(validate_membership(&cohort, &active[..4]).is_err());
        let mut wrong = active.clone();
        wrong[4] = Address::repeat_byte(99);
        assert!(validate_membership(&cohort, &wrong).is_err());
    }

    #[test]
    fn overlapping_negative_fixture_keeps_four_explicit_feeders() {
        let all = owned(&[0, 1, 2, 3, 4]);
        let active = all.iter().map(|member| member.address).collect::<Vec<_>>();
        let cohort = plan_cohort(checkpoint(), &active[..4], &all, true).unwrap();
        assert_eq!(cohort.feeder_indices, [0, 1, 2, 3]);
        assert_eq!(cohort.quorum, 3);
        assert!(plan_cohort(checkpoint(), &active, &all, true).is_err());
    }

    #[test]
    fn publication_requires_all_five_pinned_finalized_peers() {
        let good = reads();
        let ports = good.iter().map(|read| read.port).collect::<Vec<_>>();
        assert!(evaluate_publication(&ports, &good, 18, EXPECTED_RATE).unwrap());
        assert!(evaluate_publication(&ports, &good[..4], 18, EXPECTED_RATE).is_err());
        for field in 0..8 {
            let mut bad = good.clone();
            match field {
                0 => bad[4].port = 8000,
                1 => bad[4].finalized = 19,
                2 => bad[4].checkpoint.block_hash = alloy_primitives::B256::ZERO,
                3 => bad[4].checkpoint.state_root = alloy_primitives::B256::ZERO,
                4 => bad[4].rate += U256::ONE,
                5 => bad[4].volume = None,
                6 => bad[4].oracle_block = 18,
                _ => bad[4].finalized_timestamp += 1,
            }
            assert!(
                evaluate_publication(&ports, &bad, 18, EXPECTED_RATE).is_err(),
                "field {field}"
            );
        }
    }

    #[test]
    fn publication_advance_ttl_and_future_time_are_not_masked() {
        let good = reads();
        let ports = good.iter().map(|read| read.port).collect::<Vec<_>>();
        assert!(!evaluate_publication(&ports, &good, 19, EXPECTED_RATE).unwrap());
        assert!(!evaluate_publication(&ports, &good, 18, U256::ONE).unwrap());
        let mut empty = good.clone();
        for read in &mut empty {
            read.volume = None;
        }
        assert!(!evaluate_publication(&ports, &empty, 18, EXPECTED_RATE).unwrap());
        for (age, expected) in [(FX_TTL_SECS, true), (FX_TTL_SECS + 1, false)] {
            let mut changed = good.clone();
            for read in &mut changed {
                read.finalized_timestamp = read.oracle_timestamp + age;
            }
            assert_eq!(
                evaluate_publication(&ports, &changed, 18, EXPECTED_RATE).unwrap(),
                expected
            );
        }
        let mut future = good;
        for read in &mut future {
            read.oracle_timestamp = read.finalized_timestamp + 1;
        }
        assert!(evaluate_publication(&ports, &future, 18, EXPECTED_RATE).is_err());
    }

    #[test]
    fn restart_barrier_excludes_unrecorded_pre_restart_publication() {
        assert_eq!(publication_lower_bound(Some(10), 18), 18);
        assert_eq!(publication_lower_bound(Some(22), 18), 22);
        let mut old = reads();
        for read in &mut old {
            read.oracle_block = 18;
        }
        let ports = old.iter().map(|read| read.port).collect::<Vec<_>>();
        assert!(!evaluate_publication(
            &ports,
            &old,
            publication_lower_bound(Some(10), 18),
            EXPECTED_RATE
        )
        .unwrap());
    }

    #[test]
    fn controlled_quote_uses_the_canonical_six_decimal_scale() {
        assert_eq!(MOCK_PRICE, "1.000000");
        assert_eq!(EXPECTED_RATE, U256::from(1_000_000));
        assert_eq!(FX_TTL_SECS, 6 * 60 * 60);
        assert_eq!(scale6_quote(U256::from(1_080_001_u64)), "1.080001");
        assert_eq!(scale6_quote(U256::from(2_u64)), "0.000002");
    }

    #[test]
    fn feeder_restart_preserves_the_current_controlled_quote() {
        assert_eq!(
            feeder_start_quote(None),
            (MOCK_PRICE.to_owned(), MOCK_VOLUME.to_owned())
        );
        assert_eq!(
            feeder_start_quote(Some(("1.080001".into(), "77.000000".into()))),
            ("1.080001".into(), "77.000000".into())
        );
        assert_eq!(
            feeder_restart_expected_rate(Some(("1.080001".into(), "77.000000".into()))),
            U256::from(1_080_001_u64)
        );
        assert_eq!(feeder_restart_expected_rate(None), EXPECTED_RATE);
    }

    #[test]
    fn process_feeder_count_is_ceiling_two_thirds() {
        let expected = [0usize, 1, 2, 2, 3, 4, 4, 5, 6, 6, 7];
        for (active, expected_quorum) in expected.into_iter().enumerate() {
            assert_eq!(oracle_quorum(active), expected_quorum, "N={active}");
        }
    }

    #[test]
    fn overlap_fixture_is_a_ab_ab_b_and_uses_two_stablecoin_sources_for_coen() {
        let pairs = (0..4).map(overlap_pairs).collect::<Vec<_>>();
        assert_eq!(pairs.iter().map(Vec::len).collect::<Vec<_>>(), [1, 2, 2, 1]);
        assert_eq!(
            pairs[0][0]
                .sources
                .iter()
                .map(|source| source.quote)
                .collect::<Vec<_>>(),
            ["USDT", "USDC"]
        );
        assert_eq!(pairs[1][1].sources[0].volume, "10");
        assert_eq!(pairs[2][1].sources[0].volume, "20");
        assert_eq!(pairs[3][0].sources[0].volume, "30");
    }
}
