//! Real price-feeder acceptance steps over a harness-owned HTTP source.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{address, Address, U256};
use cucumber::{given, then};
use eyre::{ensure, eyre, Result};

use crate::internal::{addresses, eth};
use crate::world::localnet::{BootstrapProfile, StartOpts};
use crate::world::price_oracle::{
    CanonicalPublicationObservation, FeederLaunch, MockSourceFailure, OracleCheckpointV1,
    OracleCohortV1, OracleEvidencePhaseV1, OracleMemberV1, PenaltySnapshotEvidenceV1,
    QuorumLossEvidenceV1, QuorumLossPairEvidenceV1, ValidatorPenaltyEvidenceV1,
};
use crate::world::World;

const USD_ISO: u16 = 840;
const MOCK_PRICE: &str = "1.000000";
pub(crate) const MOCK_VOLUME: &str = "1000.000000";
pub(crate) const EXPECTED_RATE: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);
const BTC_TOKEN: Address = address!("2260fac5e5542a773aa44fbcfedf7c193bc2c599");
const FX_TTL_SECS: u64 = 21_600;

fn publication_budget(genesis: &serde_json::Value, vote_period: u64) -> Result<Duration> {
    ensure!(vote_period > 0, "Oracle vote period must be positive");
    let blocks = vote_period
        .checked_mul(4)
        .ok_or_else(|| eyre!("Oracle publication budget overflow"))?;
    super::common::consensus_block_wait_budget(genesis, blocks)
}

fn publication_deadline(world: &World, cohort: &OracleCohortV1) -> (Duration, Instant) {
    let genesis = std::fs::read(world.localnet.scenario_dir().join("genesis.json"))
        .expect("read executed Oracle scenario genesis");
    let genesis = serde_json::from_slice(&genesis).expect("decode Oracle scenario genesis");
    let vote_period = cohort_vote_period(world, cohort).expect("canonical Oracle vote period");
    let timeout = publication_budget(&genesis, vote_period)
        .expect("Oracle wait budget from executed genesis timing");
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("Oracle publication deadline fits the monotonic clock");
    (timeout, deadline)
}

#[then("live feeders reject failed and malformed sources without publishing a new price")]
fn live_feeders_reject_bad_sources(world: &mut World) {
    let usd = outbe_primitives::asset_type::currency_address(USD_ISO);
    start_feeder(world, OracleEvidencePhaseV1::Initial);
    let cohort = world.price_oracle.cohort().expect("active Oracle cohort");
    let vote_period = cohort_vote_period(world, &cohort).expect("Oracle vote period");
    wait_for_unanimous_pair_publication(
        world,
        OracleEvidencePhaseV1::Initial,
        ExpectedPublication {
            base: Address::ZERO,
            quote: usd,
            strictly_after_block: cohort.checkpoint.height,
            expected_rate: EXPECTED_RATE,
        },
        true,
    );
    let original_pids = world
        .price_oracle
        .evidence_snapshot()
        .feeder_processes
        .iter()
        .map(|process| process.pid)
        .collect::<Vec<_>>();
    for failure in [
        MockSourceFailure::HttpError,
        MockSourceFailure::InvalidPayload,
        MockSourceFailure::Timeout,
    ] {
        world
            .price_oracle
            .set_source_failure(failure)
            .expect("set controlled source failure");
        // A vote assembled before the source switch can still finalize once.
        wait_finalized_blocks(world, vote_period * 2 + 2);
        let before = world
            .rpc
            .oracle_rate_data(world.validators.primary_port(), USD_ISO)
            .expect("read last finalized price during source failure");
        let requests_before = world.price_oracle.evidence_snapshot().ticker_requests;
        let finalized_before = world
            .rpc
            .finalized(world.validators.primary_port())
            .expect("read finality during source failure");
        wait_until_finalized_height(world, finalized_before + vote_period + 2);
        world
            .price_oracle
            .ensure_cohort_feeders_alive()
            .expect("source failure must not kill feeders");
        let evidence = world.price_oracle.evidence_snapshot();
        assert!(
            evidence.ticker_requests > requests_before,
            "feeders stopped polling {failure:?}"
        );
        assert_eq!(
            evidence
                .feeder_processes
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>(),
            original_pids,
            "source failure restarted a feeder"
        );
        for port in world.validators.committee_ports() {
            let after = world
                .rpc
                .oracle_rate_data(port, USD_ISO)
                .expect("read finalized price on every validator");
            assert_eq!(
                after.last_block, before.last_block,
                "{failure:?} produced a new Oracle publication on port {port}"
            );
            assert_eq!(
                after.rate, before.rate,
                "{failure:?} changed the finalized Oracle rate on port {port}"
            );
        }
        world.price_oracle.record_cohort_observation(serde_json::json!({
            "kind": "source_failure_window", "failure": format!("{failure:?}"),
            "finalized_before": finalized_before, "finalized_after": finalized_before + vote_period + 2,
            "last_oracle_block": before.last_block, "ticker_requests_before": requests_before,
            "ticker_requests_after": evidence.ticker_requests, "feeder_pids": original_pids,
        }));
    }
    let before_recovery = world
        .rpc
        .oracle_rate_data(world.validators.primary_port(), USD_ISO)
        .expect("read price before source recovery")
        .last_block;
    world
        .price_oracle
        .publish_quote(
            OracleEvidencePhaseV1::SourceRecovery,
            "1.250000",
            MOCK_VOLUME,
        )
        .expect("publish recovered source quote");
    world
        .price_oracle
        .set_source_failure(MockSourceFailure::Healthy)
        .expect("restore price sources");
    wait_for_unanimous_pair_publication(
        world,
        OracleEvidencePhaseV1::SourceRecovery,
        ExpectedPublication {
            base: Address::ZERO,
            quote: usd,
            strictly_after_block: before_recovery,
            expected_rate: U256::from(1_250_000_u64),
        },
        true,
    );
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingPricePublication {
    strictly_after_block: u64,
    expected_rate: U256,
    deadline: Instant,
    timeout: Duration,
}

impl PendingPricePublication {
    /// The caller's joint barrier must allow this already bounded publication wait.
    pub(crate) fn remaining_wait(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

#[given(expr = "a fresh price oracle localnet with a {int}-block voting window")]
fn fresh_price_oracle_localnet(world: &mut World, window: u64) {
    let profile = BootstrapProfile::default()
        .with_governance_voting_window(window)
        .expect("valid genesis governance voting window")
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
        ExpectedPublication {
            base: Address::ZERO,
            quote: usd,
            strictly_after_block: before,
            expected_rate: EXPECTED_RATE,
        },
        true,
    );
    wait_for_unanimous_pair_publication(
        world,
        OracleEvidencePhaseV1::Initial,
        ExpectedPublication {
            base: BTC_TOKEN,
            quote: usd,
            strictly_after_block: before_b,
            expected_rate: U256::from(133_333_333_333_333_333_333u128),
        },
        true,
    );

    let expected_volume = U256::from(15u64) * outbe_primitives::units::SCALE_1E18;
    for port in world.validators.committee_ports() {
        assert_eq!(
            world.rpc.oracle_latest_volume(port, BTC_TOKEN, usd),
            Some(expected_volume),
            "target volume must be the median of the cross winners"
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
        ExpectedPublication {
            base: Address::ZERO,
            quote: usd,
            strictly_after_block: checkpoint_a.last_block,
            expected_rate: EXPECTED_RATE,
        },
        true,
    );
    wait_for_unanimous_pair_publication(
        world,
        OracleEvidencePhaseV1::QuorumRecovery,
        ExpectedPublication {
            base: BTC_TOKEN,
            quote: usd,
            strictly_after_block: checkpoint_b.last_block,
            expected_rate: U256::from(133_333_333_333_333_333_333u128),
        },
        true,
    );
}

#[then("the controlled COEN USD quote is finalized through the real price feeder")]
fn controlled_quote_is_finalized(world: &mut World) {
    start_feeder(world, OracleEvidencePhaseV1::Initial);
    assert_controlled_quote_is_finalized(world);
}

/// The controlled COEN/840 quote and every fixed pair the scenario configured, each
/// finalized unanimously by the real feeders.
#[then("the controlled COEN quotes are finalized through the real price feeder")]
fn controlled_quotes_are_finalized(world: &mut World) {
    start_feeder(world, OracleEvidencePhaseV1::Initial);
    assert_controlled_quote_is_finalized(world);
    let after = world
        .price_oracle
        .cohort()
        .expect("initial Oracle cohort")
        .checkpoint
        .height;
    for pair in world.price_oracle.fixed_pairs().to_vec() {
        wait_for_unanimous_pair_publication(
            world,
            OracleEvidencePhaseV1::Initial,
            ExpectedPublication {
                base: Address::ZERO,
                quote: outbe_primitives::asset_type::currency_address(pair.iso_code),
                strictly_after_block: after,
                expected_rate: pair.rate,
            },
            true,
        );
    }
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
    let initial_cohort = resolve_cohort(world, false).expect("initial Oracle admission cohort");
    let (_, deadline) = publication_deadline(world, &initial_cohort);
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
    let (timeout, deadline) = publication_deadline(world, &cohort);
    Some(PendingPricePublication {
        strictly_after_block: publication_lower_bound(
            Some(previous_block),
            cohort.checkpoint.height,
        ),
        expected_rate,
        deadline,
        timeout,
    })
}

/// Poll one post-restart publication without hiding an irreversible lifecycle
/// transition behind a nested wait. The caller owns the joint WWD/Oracle
/// barrier and stops polling after the first successful observation.
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
        "controlled quote did not publish a newer unanimous finalized Oracle rate within {:?} after block {}",
        pending.timeout,
        pending.strictly_after_block,
    );
    false
}

/// Atomically change the harness-owned quote and prove that the production
/// feeder published the new exact scale-6 rate on every validator.
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
            let validator_key = validator.evm_key().unwrap_or_else(|error| {
                panic!("validator-{validator_index} EVM key for feeder: {error:#}")
            });
            let validator_address = world
                .rpc
                .address_of(&validator_key)
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
            let private_key = delegated_feeder_key(world, validator_index);
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
    let validator_key = validator.evm_key().unwrap_or_else(|error| {
        panic!("validator-{validator_index} EVM key for feeder: {error:#}")
    });
    let validator_address = world
        .rpc
        .address_of(&validator_key)
        .unwrap_or_else(|| panic!("derive validator-{validator_index} feeder address"));
    let rpc_url = world.rpc.url(world.validators.http_port(validator_index));
    let private_key = delegated_feeder_key(world, validator_index);
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

fn feeder_private_key(validator: Address) -> String {
    let mut material = b"outbe/e2e/oracle-feeder/v1".to_vec();
    material.extend_from_slice(validator.as_slice());
    let mut key = alloy_primitives::keccak256(material).0;
    key[0] = (key[0] & 0x3f) | 0x40;
    format!("0x{}", hex::encode(key))
}

fn delegated_feeder_key(world: &mut World, validator_index: usize) -> String {
    let validator_key = world
        .validators
        .get(validator_index)
        .evm_key()
        .expect("Oracle owner key");
    let validator = eth::address_of(&validator_key).expect("Oracle owner address");
    let key = feeder_private_key(validator);
    let feeder = eth::address_of(&key).expect("dedicated feeder address");
    assert_ne!(
        feeder, validator,
        "Oracle signer must have an independent nonce"
    );
    let url = world.rpc.url(world.validators.http_port(validator_index));
    let ports = world.validators.committee_ports();
    let binding = eth::IOracle::getFeederDelegationCall { validator };
    if eth::read_call(&url, outbe_primitives::addresses::ORACLE_ADDRESS, &binding) != Some(feeder) {
        let outcome = eth::send_call_outcome(
            &url,
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &validator_key,
            &eth::IOracle::delegateFeederConsentCall { feeder },
            None,
        )
        .expect("register dedicated Oracle feeder through standard delegation");
        assert!(
            outcome.success,
            "Oracle delegation reverted: {}",
            outcome.receipt
        );
        world
            .rpc
            .finalize_outcome(&outcome.into(), &ports, 120)
            .expect("Oracle feeder delegation finality");
    }
    let height = ports
        .iter()
        .map(|port| {
            world
                .rpc
                .finalized(*port)
                .expect("Oracle delegation checkpoint")
        })
        .min()
        .expect("Oracle committee exists");
    for port in ports {
        assert_eq!(
            eth::read_call_at_result(
                &world.rpc.url(port),
                outbe_primitives::addresses::ORACLE_ADDRESS,
                &binding,
                height,
            )
            .expect("finalized Oracle feeder delegation"),
            feeder
        );
    }
    world
        .price_oracle
        .record_cohort_observation(serde_json::json!({
            "kind": "feeder_delegation_verified", "validator_index": validator_index,
            "validator": validator, "feeder": feeder, "finalized_height": height,
        }));
    key
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

#[derive(Clone, Copy)]
struct ExpectedPublication {
    base: Address,
    quote: Address,
    strictly_after_block: u64,
    expected_rate: U256,
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
    let published = first.volume.is_some() && first.rate == expected_rate;
    let age = first.finalized_timestamp - first.oracle_timestamp;
    let fresh = first.oracle_block > after && first.oracle_timestamp > 0 && age <= FX_TTL_SECS;
    Ok(published && fresh)
}

fn feeder_start_quote(current: Option<(String, String)>) -> (String, String) {
    current.unwrap_or_else(|| (MOCK_PRICE.to_owned(), MOCK_VOLUME.to_owned()))
}

fn feeder_restart_expected_rate(current: Option<(String, String)>) -> U256 {
    let (price, _) = feeder_start_quote(current);
    parse_scale6_rate(&price).unwrap_or_else(|| {
        panic!("controlled feeder quote `{price}` is not a canonical scale-6 rate")
    })
}

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
        ExpectedPublication {
            base: Address::ZERO,
            quote: outbe_primitives::asset_type::currency_address(USD_ISO),
            strictly_after_block,
            expected_rate,
        },
        require_live_feeder,
    );
}

fn wait_for_unanimous_pair_publication(
    world: &mut World,
    phase: OracleEvidencePhaseV1,
    expected: ExpectedPublication,
    require_live_feeder: bool,
) {
    let cohort = world
        .price_oracle
        .cohort()
        .expect("Oracle publication cohort");
    let (_, deadline) = publication_deadline(world, &cohort);
    loop {
        if observe_unanimous_pair_publication(world, phase, expected, require_live_feeder) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "controlled quote did not become one fresh finalized Oracle publication: expected_rate={} after_block={}",
            expected.expected_rate,
            expected.strictly_after_block
        );
        sleep(Duration::from_millis(250));
    }
}

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
        ExpectedPublication {
            base: Address::ZERO,
            quote: outbe_primitives::asset_type::currency_address(USD_ISO),
            strictly_after_block,
            expected_rate,
        },
        require_live_feeder,
    )
}

fn observe_unanimous_pair_publication(
    world: &mut World,
    phase: OracleEvidencePhaseV1,
    expected: ExpectedPublication,
    require_live_feeder: bool,
) -> bool {
    let result = observe_cohort_publication(world, phase, expected, require_live_feeder);
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
    expected: ExpectedPublication,
    require_live_feeder: bool,
) -> Result<bool> {
    let ExpectedPublication {
        base,
        quote,
        strictly_after_block,
        expected_rate,
    } = expected;
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
    let cohort = resolve_cohort(world, false).expect("Oracle block-wait cohort");
    let (_, deadline) = publication_deadline(world, &cohort);
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

fn scale6_quote(rate: U256) -> String {
    let scale = U256::from(1_000_000_u64);
    let whole = rate / scale;
    let fraction = rate % scale;
    format!("{whole}.{fraction:06}")
}

#[cfg(test)]
mod tests;
