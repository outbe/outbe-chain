//! Real price-feeder acceptance steps over a harness-owned HTTP source.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{address, Address, U256};
use cucumber::{given, then};

use crate::world::localnet::{BootstrapProfile, StartOpts};
use crate::world::price_oracle::{
    CanonicalPublicationObservation, FeederLaunch, OracleEvidencePhaseV1,
    PenaltySnapshotEvidenceV1, QuorumLossEvidenceV1, QuorumLossPairEvidenceV1,
    ValidatorPenaltyEvidenceV1,
};
use crate::world::World;

const USD_ISO: u16 = 840;
const MOCK_PRICE: &str = "1.000000";
const MOCK_VOLUME: &str = "1000.000000";
const EXPECTED_RATE: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);
const BTC_TOKEN: Address = address!("2260fac5e5542a773aa44fbcfedf7c193bc2c599");
const FX_TTL_SECS: u64 = 21_600;
const PUBLICATION_TIMEOUT: Duration = Duration::from_secs(180);

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
    let before = world
        .rpc
        .oracle_rate_data(world.validators.primary_port(), USD_ISO)
        .map_or(0, |rate| rate.last_block);
    start_feeder(world, OracleEvidencePhaseV1::Initial);
    wait_for_unanimous_publication(
        world,
        OracleEvidencePhaseV1::Initial,
        before,
        EXPECTED_RATE,
        true,
    );
    let evidence = world.price_oracle.evidence_snapshot();
    assert!(evidence.ticker_requests > 0);
    assert!(evidence.candle_requests > 0);
    let quorum = oracle_quorum(world.validators.size());
    assert_eq!(evidence.feeder_processes.len(), quorum);
    let mut distinct_pids = evidence
        .feeder_processes
        .iter()
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
    if !world.price_oracle.is_feeder_running() {
        return None;
    }
    world
        .price_oracle
        .ensure_feeder_alive()
        .expect("price feeder stays live before clock restart");
    let previous = world.price_oracle.last_oracle_block().unwrap_or(0);
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
    Some(PendingPricePublication {
        strictly_after_block: previous_block,
        expected_rate,
        deadline: Instant::now() + PUBLICATION_TIMEOUT,
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
        "controlled quote did not publish a newer unanimous finalized Oracle rate within {PUBLICATION_TIMEOUT:?} after block {}",
        pending.strictly_after_block,
    );
    false
}

/// Atomically change the harness-owned quote and prove that the production
/// feeder published the new exact scale-6 rate on every validator.
pub(crate) fn publish_controlled_quote(world: &mut World, expected_rate: U256) {
    let strictly_after_block = world.price_oracle.last_oracle_block().unwrap_or(0);
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
    let (price, volume) = feeder_start_quote(world.price_oracle.read_controlled_quote());
    let validator_count = world.validators.size();
    let quorum = oracle_quorum(validator_count);
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("read feeder chain id");
    let vote_period = world
        .rpc
        .oracle_vote_period(world.validators.primary_port())
        .expect("read canonical Oracle vote period for feeder");

    let feeders = (0..quorum)
        .map(|validator_index| {
            let validator = world.validators.get(validator_index);
            let private_key = validator.evm_key().unwrap_or_else(|error| {
                panic!("validator-{validator_index} EVM key for feeder: {error:#}")
            });
            let validator_address = world
                .rpc
                .address_of(&private_key)
                .unwrap_or_else(|| panic!("derive validator-{validator_index} feeder address"));
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
    let vote_period = world
        .rpc
        .oracle_vote_period(world.validators.primary_port())
        .expect("read canonical Oracle vote period for feeder");
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
    if require_live_feeder {
        world
            .price_oracle
            .ensure_feeder_alive()
            .expect("price feeder stays alive until finalized tally");
    }
    let ports = world.validators.committee_ports();
    let finalized = ports
        .iter()
        .map(|port| world.rpc.finalized(*port))
        .collect::<Vec<_>>();
    let rates = ports
        .iter()
        .map(|port| world.rpc.oracle_rate_data_for_pair(*port, base, quote))
        .collect::<Vec<_>>();
    let volumes = ports
        .iter()
        .map(|port| world.rpc.oracle_latest_volume(*port, base, quote))
        .collect::<Vec<_>>();
    let (Some(finalized_height), Some(rate)) = (
        finalized.iter().flatten().copied().min(),
        rates.first().and_then(|rate| *rate),
    ) else {
        return false;
    };
    if !rates.iter().all(|candidate| *candidate == Some(rate))
        || rate.rate != expected_rate
        || rate.last_block <= strictly_after_block
        || rate.last_block > finalized_height
    {
        return false;
    }
    let Some(volume) = volumes.first().and_then(|volume| *volume) else {
        return false;
    };
    if !volumes.iter().all(|candidate| *candidate == Some(volume)) {
        return false;
    }
    let timestamps = ports
        .iter()
        .map(|port| world.rpc.block_timestamp(*port, finalized_height))
        .collect::<Vec<_>>();
    let Some(finalized_timestamp) = timestamps.first().and_then(|value| *value) else {
        return false;
    };
    let same_timestamp = timestamps
        .iter()
        .all(|candidate| *candidate == Some(finalized_timestamp));
    let age = finalized_timestamp.saturating_sub(rate.last_timestamp);
    if !same_timestamp || rate.last_timestamp == 0 || age > FX_TTL_SECS {
        return false;
    }
    world
        .price_oracle
        .record_canonical_publication(CanonicalPublicationObservation {
            phase,
            validator_count: ports.len(),
            base,
            quote,
            rate: rate.rate,
            volume,
            oracle_block: rate.last_block,
            oracle_timestamp: rate.last_timestamp,
            finalized_height,
            finalized_timestamp,
        });
    true
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

fn scale6_quote(rate: U256) -> String {
    let scale = U256::from(1_000_000_u64);
    let whole = rate / scale;
    let fraction = rate % scale;
    format!("{whole}.{fraction:06}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
