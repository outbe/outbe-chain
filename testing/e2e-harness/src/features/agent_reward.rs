//! Full public Tribute -> UTC AgentReward settlement -> paid Gem claim evidence.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolValue as _;
use cucumber::{then, when};
use outbe_primitives::time::timestamp_to_date_key;

use crate::features::common::restart_committee_at_logical_time;
use crate::internal::addresses;
use crate::internal::economic_reference;
use crate::internal::eth::{self, IAgentReward, SRA_POOL, WAA_POOL};
use crate::world::rpc::FinalizedCheckpoint;
use crate::world::state::{
    AgentRewardCheckpointV1, AgentRewardEconomicCheckV1, AgentRewardObservationV1,
};
use crate::world::World;

const WAA_BENEFICIARY_KEY: &str =
    "0x3333333333333333333333333333333333333333333333333333333333333333";
const SRA_BENEFICIARY_KEY: &str =
    "0x4444444444444444444444444444444444444444444444444444444444444444";
// A Gem mint costs more gas than the native transfer the claim used to do.
const BENEFICIARY_GAS_FUNDING_COEN: u64 = 25;
const AGENT_REWARD_WAIT: Duration = Duration::from_secs(300);
use outbe_primitives::time::SECONDS_PER_DAY;

#[when("a user of the registered L2 submits an encrypted Tribute with ZKP and WAA and SRA beneficiaries")]
fn submit_reward_bearing_tribute(world: &mut World) {
    assert!(
        world.state.agent_reward.is_none(),
        "single reward-bearing offer fixture"
    );
    let wwd = world.state.wwd.clone().expect("WorldwideDay set at setup");
    let funder = world.validators.get(0);
    let user_key = funder.evm_key().expect("validator-0 EVM key");
    wait_for_offering(world, &wwd);

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
    let transaction_hash = world
        .rpc
        .submit_tribute_offer_with_agent_rewards(
            &user_key,
            crate::internal::l2_fixture::FIXTURE_L2_CHAIN_ID,
            &wwd,
            &[waa_beneficiary],
            &[sra_beneficiary],
        )
        .expect("submit real encrypted reward-bearing Tribute");

    world.state.tribute_tx_hash = Some(transaction_hash);
    world.state.agent_reward = Some(AgentRewardObservationV1 {
        waa_beneficiary,
        sra_beneficiary,
        offer_execution_block_number: None,
        offer_execution_timestamp: None,
        reward_utc_day: None,
        escrow_before_settlement_coen_units: None,
        cca_before_settlement_coen_units: None,
        waa_claimable_minor: None,
        sra_claimable_minor: None,
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
        .agent_reward
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
        .agent_reward
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
        .agent_reward
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
        let (minimum_height, pending_publication) =
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
        .agent_reward
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
    let (expected_waa, expected_sra) = economic_reference::single_beneficiary_rewards(emission_day);
    let before = observation
        .before_settlement_checkpoint
        .expect("pre-settlement checkpoint");
    let ports = world.validators.committee_ports();
    let deadline = Instant::now() + AGENT_REWARD_WAIT;
    let (checkpoint, balances) = loop {
        let checkpoint = match world
            .rpc
            .wait_finalized_checkpoint(&ports, before.height, 1)
        {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                // A validator may temporarily reject RPC reads while it
                // catches up with the finalized settlement block. Retry this
                // observation after three seconds, including errors returned
                // immediately by the exact checkpoint read.
                assert!(
                    Instant::now() < deadline,
                    "common finalized AgentReward checkpoint unavailable after settlement wait: {error:#}"
                );
                sleep(Duration::from_secs(3));
                continue;
            }
        };
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
        balances.waa, expected_waa,
        "WAA differs from independent full daily pool"
    );
    assert_eq!(
        balances.sra, expected_sra,
        "SRA differs from independent full daily pool"
    );
    assert_eq!(
        balances.escrow, escrow_before,
        "AgentReward settlement moved native COEN"
    );
    // This WAA/SRA scenario has no registered CCA or originated positions.
    // Its CCA allocation therefore goes to terminal Metadosis.
    let expected_cca_delta = U256::ZERO;
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
        .agent_reward
        .as_mut()
        .expect("AgentReward beneficiary fixture");
    observation.waa_claimable_minor = Some(waa_claimable);
    observation.sra_claimable_minor = Some(sra_claimable);
    observation.economic_check = Some(AgentRewardEconomicCheckV1 {
        genesis_rewards_timestamp,
        emission_day,
        expected_waa_minor: expected_waa,
        expected_sra_minor: expected_sra,
        expected_cca_delta_coen_units: expected_cca_delta,
        checkpoint,
        validator_ports: ports,
    });
    eprintln!(
        "agent_reward_evidence stage=settled reward_utc_day={} waa_claimable={waa_claimable} sra_claimable={sra_claimable} escrow={escrow_after} cca={cca_after}",
        observation.reward_utc_day.expect("reward UTC day"),
    );
}

#[when(
    expr = "both beneficiaries claim their complete AgentReward as Gems with paid transactions, WAA issued in {int} against {int} and SRA issued in {int} against {int}"
)]
fn beneficiaries_claim_agent_rewards(
    world: &mut World,
    waa_issuance: u16,
    waa_reference: u16,
    sra_issuance: u16,
    sra_reference: u16,
) {
    let (waa_claimable, sra_claimable, escrow_before) = {
        let observation = world
            .state
            .agent_reward
            .as_ref()
            .expect("settled AgentReward observation");
        (
            observation.waa_claimable_minor.expect("WAA claimable"),
            observation.sra_claimable_minor.expect("SRA claimable"),
            native_balance(
                world,
                world.validators.primary_port(),
                addresses::AGENT_REWARD_ADDR,
            ),
        )
    };

    let waa_block_number = claim_gem_and_assert_gas_only_cost(
        world,
        WAA_BENEFICIARY_KEY,
        WAA_POOL,
        waa_claimable,
        (waa_issuance, waa_reference),
    );
    let sra_block_number = claim_gem_and_assert_gas_only_cost(
        world,
        SRA_BENEFICIARY_KEY,
        SRA_POOL,
        sra_claimable,
        (sra_issuance, sra_reference),
    );
    assert_eq!(
        native_balance(
            world,
            world.validators.primary_port(),
            addresses::AGENT_REWARD_ADDR,
        ),
        escrow_before,
        "AgentReward claims moved native COEN"
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
        .agent_reward
        .as_mut()
        .expect("AgentReward observation")
        .claim_finalized_height = Some(finalized_height);
}

#[then("the paid Gem claims clear both claimables and move no AgentReward native COEN")]
fn claims_clear_agent_reward_state(world: &mut World) {
    let observation = world
        .state
        .agent_reward
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
            "AgentReward native balance changed on validator port {port}"
        );
    }
}

/// Claims one pool as a Gem in `currencies` (issuance, reference). The reward now arrives
/// as a Gem, so the only native movement on the beneficiary is the gas it paid.
fn claim_gem_and_assert_gas_only_cost(
    world: &World,
    key: &str,
    pool: u8,
    claimable: U256,
    currencies: (u16, u16),
) -> u64 {
    let (issuance_currency, reference_currency) = currencies;
    let beneficiary = beneficiary_address(world, key);
    let primary = world.validators.primary_port();
    let before = native_balance(world, primary, beneficiary);
    let inventory_before = observe_gem_state(world, primary, || {
        gem_inventory(world, primary, beneficiary).map(Some)
    });
    let receipt = world
        .rpc
        .claim_agent_reward_gem(key, pool, issuance_currency, reference_currency)
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
    let height = receipt
        .get("blockNumber")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
        .expect("AgentReward claim receipt block");
    let ports = world.validators.committee_ports();
    world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 120)
        .expect("Gem claim finality on every validator");
    let timestamp = world
        .rpc
        .block_timestamp(primary, height)
        .expect("claim timestamp");
    let issued =
        eth::receipt_event::<eth::IGemFactory::GemIssued>(&receipt, addresses::GEM_FACTORY_ADDR);
    let mut expected_body = None;
    for port in ports {
        let (after, gem) = observe_gem_state(world, port, || {
            let after = gem_inventory(world, port, beneficiary)?;
            if after.len() < inventory_before.len() + 1 {
                return Ok(None);
            }
            if after.len() != inventory_before.len() + 1 || !after.contains(&issued.gemId) {
                return Err(
                    "claim must mint exactly its receipt Gem and preserve inventory".to_owned(),
                );
            }
            let gem = eth::read_call_result(
                &world.rpc.url(port),
                addresses::GEM_ADDR,
                &eth::IGem::getGemCall {
                    gemId: issued.gemId,
                },
            )?;
            Ok(Some((after, gem)))
        });
        let added: Vec<_> = after
            .iter()
            .filter(|id| !inventory_before.contains(id))
            .copied()
            .collect();
        assert_eq!(added.len(), 1, "claim must preserve existing Gem ownership");
        for id in &inventory_before {
            assert!(after.contains(id));
        }
        let url = world.rpc.url(port);
        // The price and profile must be stable across the claim block. This
        // makes the independent pre-state expectation valid at execution time.
        let terms = claim_gem_terms_at(&url, height - 1, reference_currency);
        assert_eq!(
            terms,
            claim_gem_terms_at(&url, height, reference_currency),
            "Gem inputs changed within claim block"
        );
        let expected_load = claimable;
        assert!(!expected_load.is_zero());
        assert!(!issued.groupKey.is_zero());
        let expected = eth::IGemFactory::GemIssued {
            gemId: added[0],
            owner: beneficiary,
            promisLoadMinor: expected_load,
            entryPriceMinor: outbe_primitives::markup::marked_down(
                terms,
                outbe_agentreward::constants::AGENT_DISCOUNT_PCT,
            )
            .expect("discounted entry price"),
            issuanceCurrency: issuance_currency,
            referenceCurrency: reference_currency,
            issuedAt: timestamp,
            callPriceMinor: gem.callPriceMinor,
            callWindow: issued.callWindow,
            callThreshold: issued.callThreshold,
            callNoticePeriod: gem.callNoticePeriod,
            positionId: U256::ZERO,
            groupKey: issued.groupKey,
        };
        assert_claimed_gem(&gem, &expected);
        assert_eq!(gem.gemId, added[0]);
        let encoded = gem.abi_encode();
        assert_eq!(
            *expected_body.get_or_insert(encoded.clone()),
            encoded,
            "Gem body parity"
        );
        super::settlement::assert_receipt_event(&receipt, addresses::GEM_FACTORY_ADDR, &expected);
        assert_eq!(
            eth::receipt_json(
                &url,
                receipt["transactionHash"].as_str().expect("claim hash")
            )
            .expect("claim receipt parity")["logs"],
            receipt["logs"]
        );
    }
    height
}

/// Sample a Gem read until it lands within one unchanged head on `port`.
fn observe_gem_state<T>(
    world: &World,
    port: u16,
    mut read: impl FnMut() -> Result<Option<T>, String>,
) -> T {
    use crate::internal::state_observation::{observe_state, StateSample};
    let deadline = Instant::now() + Duration::from_secs(120);
    observe_state(
        deadline,
        || {
            let before = world
                .rpc
                .head(port)
                .ok_or_else(|| eyre::eyre!("head before Gem read"))?;
            let observation = read();
            let after = world
                .rpc
                .head(port)
                .ok_or_else(|| eyre::eyre!("head after Gem read"))?;
            Ok(StateSample {
                before,
                observation,
                after,
            })
        },
        Instant::now,
        || sleep(Duration::from_secs(2).min(deadline.saturating_duration_since(Instant::now()))),
    )
    .unwrap_or_else(|error| panic!("Gem state on port {port}: {error:#}"))
}

fn gem_inventory(world: &World, port: u16, owner: Address) -> Result<Vec<U256>, String> {
    let url = world.rpc.url(port);
    let ids = eth::read_call_result(
        &url,
        addresses::GEM_ADDR,
        &eth::IGem::tokensByOwnerCall { owner },
    )?;
    if ids.len() > 32 {
        return Err("unbounded beneficiary Gem inventory".to_owned());
    }
    if ids
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != ids.len()
    {
        return Err("duplicate beneficiary Gem inventory entry".to_owned());
    }
    Ok(ids)
}

/// Read inputs, not Gem output: Oracle's closed-day watermark. The full reward
/// scenario deliberately requires a closed day in the claimed reference currency.
fn claim_gem_terms_at(url: &str, height: u64, reference_currency: u16) -> U256 {
    let word = |address: Address, slot: u64| -> U256 {
        serde_json::from_value(
            eth::raw_json_with_params(
                url,
                "eth_getStorageAt",
                serde_json::json!([
                    format!("{address:#x}"),
                    format!("0x{slot:x}"),
                    format!("0x{height:x}")
                ]),
            )
            .expect("historical Gem input slot"),
        )
        .expect("input slot quantity")
    };
    // Oracle schema slots 58/59 are UTC-day VWAP values/watermark.
    let day: u32 = word(outbe_primitives::addresses::ORACLE_ADDRESS, 59)
        .try_into()
        .expect("UTC date key");
    assert_ne!(
        day, 0,
        "reward claim fixture requires finalized UTC-day price"
    );
    let price = eth::read_call_at_result(
        url,
        outbe_primitives::addresses::ORACLE_ADDRESS,
        &eth::IOracle::getUtcDayVwapCall {
            base: Address::ZERO,
            quote: outbe_primitives::asset_type::currency_address(reference_currency),
            utcDay: day,
        },
        height,
    )
    .expect("independent closed UTC-day claim price");
    assert!(!price.is_zero());
    price
}

fn assert_claimed_gem(gem: &eth::IGem::GemData, expected: &eth::IGemFactory::GemIssued) {
    assert_eq!(gem.owner, expected.owner);
    assert_eq!(gem.state, 0, "agent reward Gem must be born Issued");
    assert_eq!(gem.promisLoadMinor, expected.promisLoadMinor);
    assert_eq!(gem.entryPriceMinor, expected.entryPriceMinor);
    assert_eq!(gem.issuanceCurrency, expected.issuanceCurrency);
    assert_eq!(gem.referenceCurrency, expected.referenceCurrency);
    assert_eq!(gem.issuedAt, expected.issuedAt);
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
            cca: native(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS),
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

#[cfg(test)]
mod claim_tests {
    use super::*;

    fn wallet_gem() -> eth::IGem::GemData {
        eth::IGem::GemData {
            gemId: U256::from(7),
            owner: Address::repeat_byte(1),
            seriesId: Default::default(),
            subSeriesId: 0,
            state: 0,
            promisLoadMinor: U256::from(123),
            entryPriceMinor: U256::from(1_000_001),
            issuanceCurrency: 840,
            referenceCurrency: 840,
            issuedAt: 100,
            callPriceMinor: U256::from(1_100_001),
            calledAt: 0,
            callNoticePeriod: 0,
            callWindow: 0,
            callThreshold: 0,
            settlementDeadline: 0,
            groupKey: Default::default(),
        }
    }

    fn issued_gem() -> eth::IGemFactory::GemIssued {
        eth::IGemFactory::GemIssued {
            gemId: U256::from(7),
            owner: Address::repeat_byte(1),
            promisLoadMinor: U256::from(123),
            entryPriceMinor: U256::from(1_000_001),
            issuanceCurrency: 840,
            referenceCurrency: 840,
            issuedAt: 100,
            callPriceMinor: U256::from(1_100_001),
            callWindow: 0,
            callThreshold: 0,
            callNoticePeriod: 0,
            positionId: U256::ZERO,
            groupKey: Default::default(),
        }
    }

    #[test]
    fn agent_claim_oracle_rejects_wrong_recipient_load_pool_and_price() {
        let expected = issued_gem();
        let check = |gem: &eth::IGem::GemData| assert_claimed_gem(gem, &expected);
        check(&wallet_gem());
        for mutation in 0..5 {
            let mut gem = wallet_gem();
            match mutation {
                0 => gem.owner = Address::repeat_byte(2),
                1 => gem.promisLoadMinor += U256::ONE,
                2 => gem.entryPriceMinor += U256::ONE,
                3 => gem.state = 1,
                _ => gem.referenceCurrency = 978,
            }
            assert!(
                std::panic::catch_unwind(|| check(&gem)).is_err(),
                "accepted wrong Gem field {mutation}"
            );
        }
    }
}
