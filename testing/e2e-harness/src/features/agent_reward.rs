//! Full public Tribute -> UTC AgentReward settlement -> paid Gem claim evidence.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use cucumber::{then, when};
use outbe_primitives::time::timestamp_to_date_key;
use outbe_primitives::units::native_to_protocol_floor;

use crate::features::ocomp::restart_committee_at_logical_time;
use crate::internal::addresses;
use crate::internal::eth::{self, SRA_POOL, WAA_POOL};
use crate::world::state::OcompAgentRewardObservationV1;
use crate::world::World;

const WAA_BENEFICIARY_KEY: &str =
    "0x3333333333333333333333333333333333333333333333333333333333333333";
const SRA_BENEFICIARY_KEY: &str =
    "0x4444444444444444444444444444444444444444444444444444444444444444";
// A Gem mint costs more gas than the native transfer the claim used to do.
const BENEFICIARY_GAS_FUNDING_COEN: u64 = 25;
/// `GemTypes::Wallet` — what the WAA pool mints.
const WALLET_GEM_TYPE: u8 = 3;
/// `GemTypes::Sra` — what the SRA pool mints.
const SRA_GEM_TYPE: u8 = 2;
/// `GemState::Issued`; a reward Gem qualifies later, on price.
const ISSUED_GEM_STATE: u8 = 0;
/// Agent rewards are USD-denominated by protocol policy, on both currency axes.
const AGENT_GEM_CURRENCY: u16 = 840;

const AGENT_REWARD_WAIT: Duration = Duration::from_secs(300);
const SECONDS_PER_DAY: u64 = 86_400;

#[when("an operator submits one encrypted tribute offer with WAA and SRA beneficiaries")]
fn submit_reward_bearing_tribute(world: &mut World) {
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

    for port in world.validators.committee_ports() {
        assert!(
            world.rpc.wait_finalized_at_least(port, block_number, 120),
            "validator port {port} did not finalize reward-bearing Tribute block {block_number}"
        );
        assert_eq!(
            world
                .rpc
                .get_agent_reward_claimable_balance_on(port, waa_beneficiary),
            Some(U256::ZERO),
            "WAA reward became claimable before UTC settlement on validator port {port}"
        );
        assert_eq!(
            world
                .rpc
                .get_agent_reward_claimable_balance_on(port, sra_beneficiary),
            Some(U256::ZERO),
            "SRA reward became claimable before UTC settlement on validator port {port}"
        );
    }

    let escrow_before = native_balance(world, primary, addresses::AGENT_REWARD_ADDR);
    let cca_before = native_balance(world, primary, outbe_primitives::addresses::CCA_ADDRESS);
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
    let primary = world.validators.primary_port();
    let deadline = Instant::now() + AGENT_REWARD_WAIT;
    let (waa_claimable, sra_claimable, escrow_after, cca_after) = loop {
        let waa = world
            .rpc
            .get_agent_reward_claimable_balance_on(primary, observation.waa_beneficiary);
        let sra = world
            .rpc
            .get_agent_reward_claimable_balance_on(primary, observation.sra_beneficiary);
        let escrow = native_balance(world, primary, addresses::AGENT_REWARD_ADDR);
        let cca = native_balance(world, primary, outbe_primitives::addresses::CCA_ADDRESS);
        if let (Some(waa), Some(sra)) = (waa, sra) {
            if !waa.is_zero() && !sra.is_zero() {
                break (waa, sra, escrow, cca);
            }
        }
        assert!(
            Instant::now() < deadline,
            "UTC settlement did not produce nonzero WAA/SRA rewards: waa={waa:?} sra={sra:?} escrow={escrow} cca={cca}"
        );
        sleep(Duration::from_millis(500));
    };
    let expected_escrow = escrow_before
        .checked_add(waa_claimable)
        .and_then(|value| value.checked_add(sra_claimable))
        .expect("AgentReward escrow sum");
    assert_eq!(escrow_after, expected_escrow);
    assert!(
        cca_after > cca_before,
        "CCA pool did not accrue independently"
    );

    for port in world.validators.committee_ports() {
        assert_eq!(
            world
                .rpc
                .get_agent_reward_claimable_balance_on(port, observation.waa_beneficiary),
            Some(waa_claimable),
            "WAA claimable differs on validator port {port}"
        );
        assert_eq!(
            world
                .rpc
                .get_agent_reward_claimable_balance_on(port, observation.sra_beneficiary),
            Some(sra_claimable),
            "SRA claimable differs on validator port {port}"
        );
        assert_eq!(
            native_balance(world, port, addresses::AGENT_REWARD_ADDR),
            escrow_after,
            "AgentReward escrow differs on validator port {port}"
        );
    }

    let observation = world
        .state
        .ocomp_agent_reward
        .as_mut()
        .expect("AgentReward beneficiary fixture");
    observation.waa_claimable_coen_units = Some(waa_claimable);
    observation.sra_claimable_coen_units = Some(sra_claimable);
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

fn native_balance(world: &World, port: u16, address: Address) -> U256 {
    world
        .rpc
        .balance_on(port, &format!("{address:#x}"))
        .unwrap_or_else(|| panic!("read native balance for {address:#x} on validator port {port}"))
}

/// The claim steps prove a Gem-shaped payout only by the absence of native movement.
/// This reads the Gem itself: what a beneficiary actually received.
#[then("each beneficiary holds the AgentReward Gem their pool mints")]
fn beneficiaries_hold_reward_gems(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let (waa_beneficiary, sra_beneficiary, waa_claimable, sra_claimable) = {
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
        )
    };

    for (pool, owner, claimed, gem_type) in [
        ("WAA", waa_beneficiary, waa_claimable, WALLET_GEM_TYPE),
        ("SRA", sra_beneficiary, sra_claimable, SRA_GEM_TYPE),
    ] {
        let balance = eth::read_call(
            &url,
            addresses::GEM_ADDR,
            &eth::IGem::balanceOfCall { owner },
        )
        .expect("read the beneficiary's Gem balance");
        assert_eq!(
            balance,
            U256::from(1),
            "{pool} beneficiary should hold exactly the one Gem their claim minted"
        );
        let gem_id = eth::read_call(
            &url,
            addresses::GEM_ADDR,
            &eth::IGem::tokenOfOwnerByIndexCall {
                owner,
                index: U256::ZERO,
            },
        )
        .expect("read the beneficiary's Gem id");
        let gem = eth::read_call(
            &url,
            addresses::GEM_ADDR,
            &eth::IGem::getGemStatusCall { gemId: gem_id },
        )
        .expect("read the beneficiary's Gem");

        assert_eq!(gem.owner, owner, "{pool} Gem went to the wrong owner");
        assert_eq!(gem.gemType, gem_type, "{pool} Gem carries the wrong type");
        assert_eq!(
            gem.state, ISSUED_GEM_STATE,
            "{pool} Gem should start Issued and qualify later on price"
        );
        // The claim floors native COEN to whole protocol units, and the total
        // claimable is the pool's here because each beneficiary has one pool.
        assert_eq!(
            gem.promisLoad,
            native_to_protocol_floor(claimed),
            "{pool} Gem load does not match the claimed reward"
        );
        assert!(
            !gem.entryPrice.is_zero(),
            "{pool} Gem was priced against no COEN rate"
        );
        assert_eq!(
            (gem.issuanceCurrency, gem.referenceCurrency),
            (AGENT_GEM_CURRENCY, AGENT_GEM_CURRENCY),
            "{pool} Gem should carry USD on both currency axes"
        );
    }
}
