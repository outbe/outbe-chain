//! Full public Tribute -> UTC AgentReward settlement -> paid Gem claim evidence.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use cucumber::{then, when};
use outbe_primitives::time::timestamp_to_date_key;

use crate::features::ocomp::restart_committee_at_logical_time;
use crate::internal::addresses;
use crate::internal::economic_reference;
use crate::internal::eth::{self, IAgentReward, SRA_POOL, WAA_POOL};
use crate::world::rpc::FinalizedCheckpoint;
use crate::world::state::{
    AgentRewardCheckpointV1, AgentRewardEconomicCheckV1, OcompAgentRewardObservationV1,
};
use crate::world::World;

const WAA_BENEFICIARY_KEY: &str =
    "0x3333333333333333333333333333333333333333333333333333333333333333";
const SRA_BENEFICIARY_KEY: &str =
    "0x4444444444444444444444444444444444444444444444444444444444444444";
// A Gem mint costs more gas than the native transfer the claim used to do.
const BENEFICIARY_GAS_FUNDING_COEN: u64 = 25;
const AGENT_REWARD_WAIT: Duration = Duration::from_secs(300);
const SECONDS_PER_DAY: u64 = 86_400;

#[when("an operator submits one encrypted tribute offer with WAA and SRA beneficiaries")]
fn submit_reward_bearing_tribute(world: &mut World) {
    assert!(
        world.state.ocomp_agent_reward.is_none(),
        "single reward-bearing offer fixture"
    );
    let wwd = world.state.wwd.clone().expect("WorldwideDay set at setup");
    wait_for_offering(world, &wwd);

    let funder = world.validators.get(0);
    for key in [WAA_BENEFICIARY_KEY, SRA_BENEFICIARY_KEY] {
        let funding = world
            .rpc
            .fund_key(&funder, key, BENEFICIARY_GAS_FUNDING_COEN)
            .expect("fund AgentReward beneficiary for an ordinary paid claim");
        assert!(
            world.rpc.wait_successful_receipt(&funding, 120),
            "AgentReward beneficiary funding failed: {funding}"
        );
    }

    let waa_beneficiary = beneficiary_address(world, WAA_BENEFICIARY_KEY);
    let sra_beneficiary = beneficiary_address(world, SRA_BENEFICIARY_KEY);
    let operator_key = funder.evm_key().expect("validator-0 EVM key");
    let transaction_hash = world
        .rpc
        .submit_tribute_offer_with_agent_rewards(
            &operator_key,
            &wwd,
            &[waa_beneficiary],
            &[sra_beneficiary],
        )
        .expect("submit real encrypted reward-bearing Tribute");

    world.state.tribute_tx_hash = Some(transaction_hash);
    world.state.ocomp_agent_reward = Some(OcompAgentRewardObservationV1 {
        waa_beneficiary,
        sra_beneficiary,
        offer_execution_block_number: None,
        offer_execution_timestamp: None,
        reward_utc_day: None,
        escrow_before_settlement_coen_units: None,
        cca_before_settlement_coen_units: None,
        waa_claimable_coen_units: None,
        sra_claimable_coen_units: None,
        claim_finalized_height: None,
        before_settlement_checkpoint: None,
        economic_check: None,
    });
}

#[then("the WAA and SRA beneficiaries have no reward before their execution UTC day settles")]
fn reward_is_not_available_before_utc_settlement(world: &mut World) {
    let primary = world.validators.primary_port();
    let transaction_hash = world
        .state
        .tribute_tx_hash
        .as_deref()
        .expect("reward-bearing Tribute transaction");
    let block_number = world
        .rpc
        .receipt_block_number(transaction_hash, primary)
        .expect("reward-bearing Tribute receipt block");
    let block_timestamp = world
        .rpc
        .block_timestamp(primary, block_number)
        .expect("reward-bearing Tribute execution timestamp");
    let reward_utc_day = timestamp_to_date_key(block_timestamp);
    let (waa_beneficiary, sra_beneficiary) = world
        .state
        .ocomp_agent_reward
        .as_ref()
        .map(|observation| (observation.waa_beneficiary, observation.sra_beneficiary))
        .expect("AgentReward beneficiary fixture");

    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&world.validators.committee_ports(), block_number, 120)
        .expect("finalized reward-bearing Tribute on every validator");
    let balances = reward_balances_at(world, checkpoint, waa_beneficiary, sra_beneficiary);
    assert_eq!(balances.waa, U256::ZERO, "WAA reward before UTC settlement");
    assert_eq!(balances.sra, U256::ZERO, "SRA reward before UTC settlement");
    let escrow_before = balances.escrow;
    let cca_before = balances.cca;
    let checkpoint = reward_checkpoint(world, checkpoint);
    let observation = world
        .state
        .ocomp_agent_reward
        .as_mut()
        .expect("AgentReward beneficiary fixture");
    observation.offer_execution_block_number = Some(block_number);
    observation.offer_execution_timestamp = Some(block_timestamp);
    observation.reward_utc_day = Some(reward_utc_day);
    observation.escrow_before_settlement_coen_units = Some(escrow_before);
    observation.cca_before_settlement_coen_units = Some(cca_before);
    observation.before_settlement_checkpoint = Some(checkpoint);
    eprintln!(
        "agent_reward_evidence stage=offered tx={transaction_hash} execution_block={block_number} execution_timestamp={block_timestamp} reward_utc_day={reward_utc_day} waa={:#x} sra={:#x} escrow_before={escrow_before} cca_before={cca_before}",
        waa_beneficiary,
        sra_beneficiary,
    );
}

#[when("the offer execution UTC day reaches its next ProtocolCycle settlement")]
fn advance_to_agent_reward_settlement(world: &mut World) {
    let execution_timestamp = world
        .state
        .ocomp_agent_reward
        .as_ref()
        .and_then(|observation| observation.offer_execution_timestamp)
        .expect("reward-bearing Tribute execution timestamp");
    let settlement_timestamp = execution_timestamp
        .checked_div(SECONDS_PER_DAY)
        .and_then(|day| day.checked_add(1))
        .and_then(|day| day.checked_mul(SECONDS_PER_DAY))
        .and_then(|midnight| midnight.checked_add(1))
        .expect("next UTC settlement timestamp");
    let primary = world.validators.primary_port();
    let current_timestamp = world
        .rpc
        .latest_block_timestamp(primary)
        .expect("canonical timestamp before AgentReward settlement");

    if current_timestamp < settlement_timestamp {
        let (_, _, minimum_height, pending_publication) =
            restart_committee_at_logical_time(world, settlement_timestamp);
        for port in world.validators.committee_ports() {
            assert!(
                world.rpc.wait_finalized_at_least(port, minimum_height, 240),
                "validator port {port} did not finalize after AgentReward UTC transition"
            );
        }
        if let Some(pending) = pending_publication {
            while !crate::features::price_oracle::observe_pending_publication(world, &pending) {
                sleep(Duration::from_millis(500));
            }
        }
    }
}

#[then("every validator observes the same nonzero WAA and SRA AgentReward")]
fn observe_agent_rewards(world: &mut World) {
    let observation = world
        .state
        .ocomp_agent_reward
        .as_ref()
        .expect("AgentReward beneficiary fixture");
    let escrow_before = observation
        .escrow_before_settlement_coen_units
        .expect("AgentReward escrow before settlement");
    let cca_before = observation
        .cca_before_settlement_coen_units
        .expect("CCA balance before settlement");
    // The seeder anchors Rewards to config.genesisTime. Logical-clock fixtures
    // can shift the header timestamp afterwards, so block 0 is not this input.
    let genesis: serde_json::Value = serde_json::from_slice(
        &std::fs::read(world.localnet.scenario_dir().join("genesis.json"))
            .expect("executed genesis for reward calendar"),
    )
    .expect("executed genesis JSON");
    let genesis_rewards_timestamp = time::OffsetDateTime::parse(
        genesis["config"]["genesisTime"]
            .as_str()
            .expect("seeded Rewards genesisTime"),
        &time::format_description::well_known::Rfc3339,
    )
    .expect("Rewards genesisTime is RFC3339")
    .unix_timestamp();
    let genesis_rewards_timestamp =
        u64::try_from(genesis_rewards_timestamp).expect("nonnegative Rewards genesis timestamp");
    let emission_day = economic_reference::emission_day(
        genesis_rewards_timestamp,
        observation
            .offer_execution_timestamp
            .expect("offer execution time"),
    );
    let expected_reward = economic_reference::single_beneficiary_reward(emission_day);
    let expected_escrow = escrow_before
        .checked_add(expected_reward)
        .and_then(|amount| amount.checked_add(expected_reward))
        .expect("expected AgentReward escrow fits U256");
    let before = observation
        .before_settlement_checkpoint
        .expect("pre-settlement checkpoint");
    let ports = world.validators.committee_ports();
    let deadline = Instant::now() + AGENT_REWARD_WAIT;
    let (checkpoint, balances) = loop {
        let checkpoint = world
            .rpc
            .wait_finalized_checkpoint(&ports, before.height, 1)
            .expect("common finalized AgentReward checkpoint");
        let balances = reward_balances_at(
            world,
            checkpoint,
            observation.waa_beneficiary,
            observation.sra_beneficiary,
        );
        if !balances.waa.is_zero() || !balances.sra.is_zero() {
            break (reward_checkpoint(world, checkpoint), balances);
        }
        assert!(
            Instant::now() < deadline,
            "UTC settlement produced no WAA/SRA rewards"
        );
        sleep(Duration::from_millis(500));
    };
    assert_eq!(
        balances.waa, expected_reward,
        "WAA differs from independent capped daily pool"
    );
    assert_eq!(
        balances.sra, expected_reward,
        "SRA differs from independent capped daily pool"
    );
    assert_eq!(
        balances.escrow, expected_escrow,
        "AgentReward escrow differs from expected rewards"
    );
    let expected_cca_delta = economic_reference::cca_accrual(
        economic_reference::emission_day(genesis_rewards_timestamp, before.timestamp),
        economic_reference::emission_day(genesis_rewards_timestamp, checkpoint.timestamp),
    );
    assert_eq!(
        balances.cca,
        cca_before
            .checked_add(expected_cca_delta)
            .expect("expected CCA balance fits U256"),
        "CCA differs from independent daily pools"
    );
    let waa_claimable = balances.waa;
    let sra_claimable = balances.sra;
    let escrow_after = balances.escrow;
    let cca_after = balances.cca;

    let observation = world
        .state
        .ocomp_agent_reward
        .as_mut()
        .expect("AgentReward beneficiary fixture");
    observation.waa_claimable_coen_units = Some(waa_claimable);
    observation.sra_claimable_coen_units = Some(sra_claimable);
    observation.economic_check = Some(AgentRewardEconomicCheckV1 {
        genesis_rewards_timestamp,
        emission_day,
        expected_waa_coen_units: expected_reward,
        expected_sra_coen_units: expected_reward,
        expected_cca_delta_coen_units: expected_cca_delta,
        checkpoint,
        validator_ports: ports,
    });
    eprintln!(
        "agent_reward_evidence stage=settled reward_utc_day={} waa_claimable={waa_claimable} sra_claimable={sra_claimable} escrow={escrow_after} cca={cca_after}",
        observation.reward_utc_day.expect("reward UTC day"),
    );
}

#[when("both beneficiaries claim their complete AgentReward as Gems with paid transactions")]
fn beneficiaries_claim_agent_rewards(world: &mut World) {
    let (waa_beneficiary, sra_beneficiary, waa_claimable, sra_claimable, escrow_before) = {
        let observation = world
            .state
            .ocomp_agent_reward
            .as_ref()
            .expect("settled AgentReward observation");
        (
            observation.waa_beneficiary,
            observation.sra_beneficiary,
            observation.waa_claimable_coen_units.expect("WAA claimable"),
            observation.sra_claimable_coen_units.expect("SRA claimable"),
            native_balance(
                world,
                world.validators.primary_port(),
                addresses::AGENT_REWARD_ADDR,
            ),
        )
    };

    let waa_block_number =
        claim_gem_and_assert_gas_only_cost(world, WAA_BENEFICIARY_KEY, waa_beneficiary, WAA_POOL);
    let escrow_after_waa = native_balance(
        world,
        world.validators.primary_port(),
        addresses::AGENT_REWARD_ADDR,
    );
    assert_eq!(
        escrow_after_waa,
        escrow_before
            .checked_sub(waa_claimable)
            .expect("WAA escrow debit")
    );

    let sra_block_number =
        claim_gem_and_assert_gas_only_cost(world, SRA_BENEFICIARY_KEY, sra_beneficiary, SRA_POOL);
    let escrow_after_sra = native_balance(
        world,
        world.validators.primary_port(),
        addresses::AGENT_REWARD_ADDR,
    );
    assert_eq!(
        escrow_after_sra,
        escrow_after_waa
            .checked_sub(sra_claimable)
            .expect("SRA escrow debit")
    );

    let finalized_height = waa_block_number.max(sra_block_number);
    for port in world.validators.committee_ports() {
        assert!(
            world
                .rpc
                .wait_finalized_at_least(port, finalized_height, 120),
            "validator port {port} did not finalize both AgentReward claims"
        );
    }
    world
        .state
        .ocomp_agent_reward
        .as_mut()
        .expect("AgentReward observation")
        .claim_finalized_height = Some(finalized_height);
}

#[then("the paid Gem claims clear both claimables and debit the AgentReward escrow exactly")]
fn claims_clear_agent_reward_state(world: &mut World) {
    let observation = world
        .state
        .ocomp_agent_reward
        .as_ref()
        .expect("claimed AgentReward observation");
    let expected_escrow = observation
        .escrow_before_settlement_coen_units
        .expect("pre-settlement AgentReward escrow");
    assert!(observation.claim_finalized_height.is_some());

    for port in world.validators.committee_ports() {
        assert_eq!(
            world
                .rpc
                .get_agent_reward_claimable_balance_on(port, observation.waa_beneficiary),
            Some(U256::ZERO),
            "WAA claimable was not cleared on validator port {port}"
        );
        assert_eq!(
            world
                .rpc
                .get_agent_reward_claimable_balance_on(port, observation.sra_beneficiary),
            Some(U256::ZERO),
            "SRA claimable was not cleared on validator port {port}"
        );
        assert_eq!(
            native_balance(world, port, addresses::AGENT_REWARD_ADDR),
            expected_escrow,
            "AgentReward escrow was not debited exactly on validator port {port}"
        );
    }
}

/// Claims one pool as a Gem. The reward now arrives as a Gem, so the only native
/// movement on the beneficiary is the gas it paid.
fn claim_gem_and_assert_gas_only_cost(
    world: &World,
    key: &str,
    beneficiary: Address,
    pool: u8,
) -> u64 {
    let primary = world.validators.primary_port();
    let before = native_balance(world, primary, beneficiary);
    let receipt = world
        .rpc
        .claim_agent_reward_gem(key, pool)
        .expect("ordinary paid AgentReward Gem claim");
    let gas_cost_coen_units = crate::world::rpc::Rpc::receipt_gas_cost(&receipt)
        .expect("exact AgentReward claim gas cost");
    let after = native_balance(world, primary, beneficiary);
    assert_eq!(
        after
            .checked_add(gas_cost_coen_units)
            .expect("post-claim balance plus gas"),
        before,
        "the Gem claim moved native COEN beyond its own gas"
    );
    receipt
        .get("blockNumber")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
        .expect("AgentReward claim receipt block")
}

fn wait_for_offering(world: &World, wwd: &str) {
    let worldwide_day = wwd.parse::<u32>().expect("valid reward-bearing WWD");
    let primary = world.validators.primary_port();
    for _ in 0..240 {
        let state = world
            .rpc
            .metadosis_wwd_state_on(primary, worldwide_day)
            .expect("read WWD before reward-bearing Tribute");
        if state.status == 2 {
            return;
        }
        sleep(Duration::from_millis(500));
    }
    panic!("WorldwideDay {wwd} did not reach OFFERING for reward-bearing Tribute");
}

fn beneficiary_address(world: &World, key: &str) -> Address {
    world
        .rpc
        .address_of(key)
        .and_then(|address| address.parse().ok())
        .expect("derive deterministic AgentReward beneficiary")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RewardBalances {
    waa: U256,
    sra: U256,
    escrow: U256,
    cca: U256,
}

fn reward_checkpoint(world: &World, checkpoint: FinalizedCheckpoint) -> AgentRewardCheckpointV1 {
    AgentRewardCheckpointV1 {
        height: checkpoint.height,
        block_hash: checkpoint.block_hash,
        state_root: checkpoint.state_root,
        timestamp: world
            .rpc
            .block_timestamp(world.validators.primary_port(), checkpoint.height)
            .expect("AgentReward checkpoint timestamp"),
    }
}

fn reward_balances_at(
    world: &World,
    checkpoint: FinalizedCheckpoint,
    waa: Address,
    sra: Address,
) -> RewardBalances {
    let mut expected = None;
    for port in world.validators.committee_ports() {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, checkpoint.height)
                .expect("AgentReward checkpoint before reads"),
            checkpoint
        );
        let url = world.rpc.url(port);
        let claimable = |account| {
            eth::read_call_at_result(
                &url,
                addresses::AGENT_REWARD_ADDR,
                &IAgentReward::getClaimableBalanceCall { account },
                checkpoint.height,
            )
            .unwrap_or_else(|error| panic!("claimable on port {port}: {error}"))
        };
        let native = |account: Address| {
            let value = eth::raw_json_with_params(
                &url,
                "eth_getBalance",
                serde_json::json!([
                    format!("{account:#x}"),
                    format!("0x{:x}", checkpoint.height)
                ]),
            )
            .expect("native balance at finalized AgentReward checkpoint");
            U256::from_str_radix(
                value
                    .as_str()
                    .expect("native balance hex")
                    .trim_start_matches("0x"),
                16,
            )
            .expect("native balance fits U256")
        };
        let balances = RewardBalances {
            waa: claimable(waa),
            sra: claimable(sra),
            escrow: native(addresses::AGENT_REWARD_ADDR),
            cca: native(outbe_primitives::addresses::CCA_ADDRESS),
        };
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, checkpoint.height)
                .expect("AgentReward checkpoint after reads"),
            checkpoint
        );
        if let Some(expected) = expected {
            assert_eq!(
                balances, expected,
                "AgentReward balances disagree on port {port}"
            );
        } else {
            expected = Some(balances);
        }
    }
    expected.expect("nonempty AgentReward validator cohort")
}

fn native_balance(world: &World, port: u16, address: Address) -> U256 {
    world
        .rpc
        .balance_on(port, &format!("{address:#x}"))
        .unwrap_or_else(|| panic!("read native balance for {address:#x} on validator port {port}"))
}
