//! Release E2E evidence for the existing Gem and Nod settlement/redemption paths.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolEvent as _};
use cucumber::{given, then, when};
use outbe_primitives::storage::types::StorageKey as _;
use outbe_primitives::units::checked_protocol_to_native;
use outbe_tee::protocol::{GratisOp, Ledger, PromisOp};

use crate::env::environment;
use crate::features::paynote;
use crate::internal::{addresses, eth};
use crate::world::forge::{self, address_from, DEPLOYER_KEY};
use crate::world::World;

const USD_ISO: u16 = 840;
const REWARD_GEM_TIMEOUT_SECS: u64 = 900;
const MATERIALIZED_NOD_TIMEOUT_SECS: u64 = 60;
const REWARD_GEM_QUEUE_HEAD_SLOT: u64 = 29;
const REWARD_GEM_QUEUE_TAIL_SLOT: u64 = 30;
const REWARD_GEM_UTC_DAY_BY_SEQUENCE_SLOT: u64 = 31;
const DAILY_TOPUP_PREPARED_SLOT: u64 = 33;
const REWARD_GEM_RECIPIENT_COUNT_SLOT: u64 = 36;
const DAILY_TOPUP_SETTLED_SLOT: u64 = 15;
const REWARD_GEM_PENDING_BATCH_COUNT_SLOT: u64 = 42;
const MAX_REWARD_DELIVERY_SCAN_BLOCKS: u64 = 1_024;

sol! {
    interface ISettlementAsset {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
        function decimals() external view returns (uint8);
        function isoCode() external view returns (uint16);
    }

    interface ISettlementVault {
        function asset() external view returns (address);
        function owner() external view returns (address);
        function balanceOf(address account) external view returns (uint256);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SettlementFixture {
    pub(crate) asset: Address,
    pub(crate) vault: Address,
}

#[given("the committee has reached a usable finalized height")]
fn committee_has_usable_finality(world: &mut World) {
    let primary = world.validators.primary_port();
    let want = world
        .rpc
        .finalized(primary)
        .expect("read initial finalized height")
        .saturating_add(1);
    for port in world.validators.committee_ports() {
        assert!(
            world.rpc.wait_finalized_at_least(port, want, 60),
            "validator {port} did not reach usable finalized height {want}"
        );
    }
}

#[then("validator 0 receives a protocol reward Gem from same-block RewardsGemDelivery")]
fn validator_receives_reward_gem(world: &mut World) {
    let (owner, gem_id, gem) = wait_for_validator_reward_gem(world);
    assert_eq!(gem.owner, owner);
    assert!(
        gem.gemType == 0 || gem.gemType == 1,
        "validator-owned reward Gem {gem_id} has non-reward type {}",
        gem.gemType
    );
    assert_eq!(gem.state, 1, "reward Gem must be Qualified for settlement");
    assert!(
        !gem.promisLoad.is_zero(),
        "reward Gem load must be non-zero"
    );
    assert!(
        !gem.entryPrice.is_zero(),
        "reward Gem entry price must be non-zero"
    );
    let delivery_block_number = find_canonical_reward_gem_delivery_block_number(world, gem_id)
        .expect("reward Gem must be emitted by canonical CycleTick -> RewardsGemDelivery");
    eprintln!(
        "settlement_evidence kind=reward_gem owner={owner:#x} gem_id={gem_id} load={} entry={} delivery_block_number={delivery_block_number}",
        gem.promisLoad, gem.entryPrice,
    );
}

#[when("the chain crosses into the next worldwide day without a price feeder")]
fn cross_worldwide_day_without_price_feeder(world: &mut World) {
    assert!(
        !world.price_oracle.is_feeder_running(),
        "stale recovery scenario must not start a feeder before the boundary"
    );
    let port = world.validators.primary_port();
    let started_at = world
        .rpc
        .latest_block_timestamp(port)
        .expect("latest timestamp before stale daily boundary");
    let started_day = started_at / 86_400;
    world.state.reward_gem_balance_before_delivery =
        Some(validator_reward_gem_balance(world, port));

    let deadline = Instant::now() + Duration::from_secs(REWARD_GEM_TIMEOUT_SECS);
    loop {
        let timestamp = world
            .rpc
            .latest_block_timestamp(port)
            .expect("latest timestamp while awaiting stale daily boundary");
        if timestamp / 86_400 > started_day {
            let head = world
                .rpc
                .head(port)
                .expect("head after stale daily boundary");
            assert!(
                world.rpc.wait_finalized_at_least(port, head, 60),
                "daily-boundary block did not finalize: head={head} finalized={:?}",
                world.rpc.finalized(port),
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "chain did not cross the UTC boundary within {REWARD_GEM_TIMEOUT_SECS}s: started_at={started_at} latest={timestamp}"
        );
        sleep(Duration::from_millis(500));
    }
}

#[then("the stale boundary finalizes with one pending reward Gem batch and no new Gem")]
fn stale_boundary_persists_one_reward_batch(world: &mut World) {
    let port = world.validators.primary_port();
    let before = world
        .state
        .reward_gem_balance_before_delivery
        .expect("Gem balance captured before stale boundary");
    let deadline = Instant::now() + Duration::from_secs(REWARD_GEM_TIMEOUT_SECS);
    let snapshot = loop {
        let snapshot = reward_gem_queue_snapshot(world, port)
            .expect("read Rewards Gem FIFO after stale daily boundary");
        if snapshot.tail == snapshot.head.saturating_add(1) && snapshot.reward_utc_day.is_some() {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "Rewards did not persist exactly one pending batch: {snapshot:?} head={:?} finalized={:?}",
            world.rpc.head(port),
            world.rpc.finalized(port),
        );
        sleep(Duration::from_millis(500));
    };
    let reward_utc_day = snapshot.reward_utc_day.expect("pending FIFO head UTC day");
    assert!(snapshot.is_prepared, "pending reward day must be prepared");
    assert!(
        !snapshot.is_settled,
        "stale reward day must not be delivered"
    );
    assert_eq!(snapshot.recipient_count, 4, "one obligation per validator");
    assert_eq!(validator_reward_gem_balance(world, port), before);
    assert_reward_gem_queue_parity(world, snapshot);
    world.state.pending_reward_gem_utc_day = Some(reward_utc_day);
    eprintln!(
        "settlement_evidence kind=reward_gem_pending reward_utc_day={reward_utc_day} head={} tail={} recipients={} finalized={:?}",
        snapshot.head,
        snapshot.tail,
        snapshot.recipient_count,
        world.rpc.finalized(port),
    );
}

#[when("the committee restarts while the reward Gem batch is pending")]
fn restart_committee_with_pending_reward_batch(world: &mut World) {
    let port = world.validators.primary_port();
    let before = world
        .rpc
        .finalized(port)
        .expect("finalized height before pending Rewards restart");
    let pending = reward_gem_queue_snapshot(world, port).expect("pending batch before restart");
    assert_eq!(
        pending.reward_utc_day,
        world.state.pending_reward_gem_utc_day
    );
    world
        .localnet
        .restart_committee_and_enclaves()
        .expect("restart committee while Rewards batch is pending");
    assert!(
        world
            .rpc
            .wait_finalized_at_least(port, before.saturating_add(1), 90),
        "committee did not resume finality after pending Rewards restart"
    );
    let after = reward_gem_queue_snapshot(world, port).expect("pending batch after restart");
    assert_eq!(after, pending, "restart changed the pending Rewards batch");
    assert_reward_gem_queue_parity(world, after);
    world.state.pending_reward_gem_restart_block_number = Some(before);
}

#[then("the first canonical fresh tally delivers the saved reward Gem batch exactly once")]
fn fresh_tally_delivers_saved_reward_batch(world: &mut World) {
    let port = world.validators.primary_port();
    let before = world
        .state
        .reward_gem_balance_before_delivery
        .expect("Gem balance captured before pending delivery");
    let expected = before
        .checked_add(U256::ONE)
        .expect("Gem balance increment");
    let deadline = Instant::now() + Duration::from_secs(REWARD_GEM_TIMEOUT_SECS);
    loop {
        if validator_reward_gem_balance(world, port) == expected {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fresh canonical rate did not deliver the pending validator Gem: before={before} current={} head={:?} finalized={:?}",
            validator_reward_gem_balance(world, port),
            world.rpc.head(port),
            world.rpc.finalized(port),
        );
        sleep(Duration::from_millis(500));
    }
    let snapshot = reward_gem_queue_snapshot(world, port).expect("Rewards FIFO after delivery");
    assert_eq!(
        snapshot.head, snapshot.tail,
        "delivery did not pop FIFO head"
    );
    let gem_id = reward_gem_id_at(world, port, expected - U256::ONE);
    let fresh_tally_block_number = world
        .price_oracle
        .last_oracle_block()
        .expect("controlled feeder recorded one canonical fresh tally");
    let delivery_block_numbers = canonical_reward_gem_delivery_block_numbers(world, gem_id);
    assert_eq!(
        delivery_block_numbers,
        vec![fresh_tally_block_number],
        "the saved batch must be delivered exactly once by the OSG2 in the first canonical fresh tally block"
    );
    let delivery_block_number = fresh_tally_block_number;
    world.state.reward_gem_delivery_block_number = Some(delivery_block_number);
    world.state.delivered_reward_gem_id = Some(gem_id);
    eprintln!(
        "settlement_evidence kind=reward_gem_recovered reward_utc_day={} gem_id={gem_id} delivery_block_number={delivery_block_number}",
        world
            .state
            .pending_reward_gem_utc_day
            .expect("pending reward UTC day"),
    );
}

#[then("every validator observes the same delivered reward Gem and continued finality")]
fn every_validator_observes_one_reward_delivery(world: &mut World) {
    let primary = world.validators.primary_port();
    let expected_balance = world
        .state
        .reward_gem_balance_before_delivery
        .expect("Gem balance before recovered delivery")
        + U256::ONE;
    let delivered = world
        .state
        .delivered_reward_gem_id
        .expect("delivered reward Gem id");
    let delivery_block_number = world
        .state
        .reward_gem_delivery_block_number
        .expect("delivery block number");
    for port in world.validators.committee_ports() {
        assert_eq!(validator_reward_gem_balance(world, port), expected_balance);
        assert_eq!(
            reward_gem_id_at(world, port, expected_balance - U256::ONE),
            delivered,
            "validator {port} observes a different delivered Gem"
        );
        let queue = reward_gem_queue_snapshot(world, port).expect("Rewards FIFO parity read");
        assert_eq!(
            queue.head, queue.tail,
            "validator {port} retains pending batch"
        );
    }
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, delivery_block_number.saturating_add(3), 60),
        "finality did not continue after reward Gem delivery"
    );
    assert_eq!(
        validator_reward_gem_balance(world, primary),
        expected_balance
    );
    assert_eq!(
        canonical_reward_gem_delivery_block_numbers(world, delivered),
        vec![delivery_block_number],
        "continued finality must not create a second delivery for the saved batch"
    );
}

#[then("validator 0 with zero COEN uses ZeroFee to redeem that Gem into exact COEN")]
fn validator_redeems_reward_gem(world: &mut World) {
    let validator = world.validators.get(0);
    let key = validator.evm_key().expect("validator 0 EVM key");
    let payer_key = world
        .validators
        .get(1)
        .evm_key()
        .expect("validator 1 sponsorship payer key");
    let payer = eth::address_of(&payer_key).expect("validator 1 payer address");
    let (owner, gem_id, gem) = wait_for_validator_reward_gem(world);
    let fixture = deploy_settlement_fixture(world);
    let url = world.rpc.url(world.validators.primary_port());
    // The cost is derived, so what to fund is the factory's own quote — already in
    // the settlement asset's units, which the reference amount never was.
    let payable = eth::read_call(
        &url,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::quoteSettlementCall {
            gemId: gem_id,
            asset: fixture.asset,
        },
    )
    .expect("quote settling the reward Gem")
    .payableUnits;
    // The vault is credited here, not at settle time. Before the drain: this is an
    // ordinary transaction and pays its own gas.
    let paynote_proof = paynote::deposit_and_prove(
        world,
        world.validators.primary_port(),
        &key,
        owner,
        fixture.asset,
        u128::try_from(payable).expect("Gem cost fits a PayNote spend amount"),
    );
    assert_eq!(
        eth::read_call(
            &url,
            fixture.asset,
            &ISettlementAsset::balanceOfCall {
                account: fixture.vault,
            },
        ),
        Some(payable),
        "reserve vault did not receive exact Gem cost at deposit time"
    );
    let keys =
        eth::derive_account_keys(&url, &key, Ledger::Promis).expect("derive validator Promis keys");

    let drain = eth::drain_native_balance(&url, &key, payer)
        .expect("drain validator spendable COEN before ZeroFee proof");
    assert_mined_success(&drain, "drain validator spendable COEN");
    assert_eq!(
        eth::balance(&url, owner),
        Some(U256::ZERO),
        "validator must enter the sponsored redemption path with exactly zero COEN"
    );

    let delegation =
        eth::install_delegation_for_authority(&url, &payer_key, &key, addresses::ZEROFEE_ADDR)
            .expect("install sponsor-paid ZeroFee delegation for zero-balance validator");
    assert_eq!(
        delegation.get("status").and_then(serde_json::Value::as_str),
        Some("0x1"),
        "sponsor-paid delegation reverted: {delegation}"
    );
    assert_eq!(
        eth::balance(&url, owner),
        Some(U256::ZERO),
        "delegation payer, not validator, must pay installation gas"
    );
    assert_eq!(
        eth::read_call(
            &url,
            addresses::ZEROFEE_ADDR,
            &eth::IZeroFee::authorizeSponsorshipCall { signer: owner },
        ),
        Some(true),
        "ZeroFee must authorize an under-quota address at zero native balance"
    );
    let counter_before = eth::read_call(
        &url,
        addresses::ZEROFEE_ADDR,
        &eth::IZeroFee::getCounterCall { signer: owner },
    )
    .expect("ZeroFee counter before Gem redemption");
    assert_eq!(counter_before.count, 0);

    let settle = eth::send_sponsored_call(
        &url,
        &key,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::settleGemCall {
            gemId: gem_id,
            payNoteProof: paynote_proof.into(),
        },
    )
    .expect("sponsored settle reward Gem");
    assert_mined_success(&settle, "sponsored settle reward Gem");
    assert_eq!(eth::balance(&url, owner), Some(U256::ZERO));

    let promis_before = promis_balance(&url, owner, &keys.view);
    let promis_nonce = eth::read_call(
        &url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::opNonceOfCall { account: owner },
    )
    .expect("Promis nonce before Gem mining");
    let chain_id = chain_id_b256(world);
    let mint_mac = outbe_tee_enclave::promis::modify_mac(
        &keys.modify,
        owner,
        PromisOp::Mint,
        gem.promisLoad,
        promis_nonce,
        chain_id,
    );
    let pow = find_pow_nonce(gem_id);
    let mine_promis = eth::send_sponsored_call(
        &url,
        &key,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::minePromisCall {
            gemId: gem_id,
            nonce: pow,
            mac: B256::from(mint_mac),
            opNonce: promis_nonce,
        },
    )
    .expect("sponsored mine Promis from settled Gem");
    assert_mined_success(&mine_promis, "sponsored mine Promis from settled Gem");
    assert_eq!(eth::balance(&url, owner), Some(U256::ZERO));
    assert_eq!(
        promis_balance(&url, owner, &keys.view),
        promis_before + gem.promisLoad,
        "Gem load was not minted exactly into validator Promis"
    );

    let burn_nonce = eth::read_call(
        &url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::opNonceOfCall { account: owner },
    )
    .expect("Promis nonce before COEN mining");
    let burn_mac = outbe_tee_enclave::promis::modify_mac(
        &keys.modify,
        owner,
        PromisOp::Burn,
        gem.promisLoad,
        burn_nonce,
        chain_id,
    );
    let mine_coen = eth::send_sponsored_call(
        &url,
        &key,
        addresses::PROMIS_FACTORY_ADDR,
        &eth::IPromisFactory::mineCoenCall {
            amount: gem.promisLoad,
            mac: B256::from(burn_mac),
            opNonce: burn_nonce,
        },
    )
    .expect("sponsored mine COEN from validator Promis");
    assert!(
        mine_coen.success,
        "mine COEN from validator Promis reverted: {}",
        mine_coen.receipt
    );
    let native_after = eth::balance(&url, owner).expect("native balance after Promis burn");
    assert_eq!(promis_balance(&url, owner, &keys.view), promis_before);
    assert_eq!(
        native_after,
        checked_protocol_to_native(gem.promisLoad).expect("Gem load fits native COEN"),
        "three sponsored calls must charge no native fee to the validator"
    );
    let counter_after = eth::read_call(
        &url,
        addresses::ZEROFEE_ADDR,
        &eth::IZeroFee::getCounterCall { signer: owner },
    )
    .expect("ZeroFee counter after Gem redemption");
    assert_eq!(
        counter_after.count, 3,
        "settleGem, minePromis, and mineCoen must consume three of eight sponsored slots"
    );
    eprintln!(
        "settlement_evidence kind=zerofee_gem_to_coen owner={owner:#x} payer={payer:#x} gem_id={gem_id} asset={:#x} vault={:#x} amount={} settle_tx={} promis_tx={} coen_tx={} quota_used={} native_before=0 native_after={}",
        fixture.asset, fixture.vault, gem.promisLoad, settle.transaction_hash, mine_promis.transaction_hash, mine_coen.transaction_hash, counter_after.count, native_after
    );
}

#[then("the public Tribute owner settles its Nod and redeems its exact Gratis into COEN")]
fn owner_redeems_materialized_nod(world: &mut World) {
    let key = world
        .validators
        .get(0)
        .evm_key()
        .expect("public Tribute owner key");
    let owner = world
        .rpc
        .address_of(&key)
        .expect("public Tribute owner address")
        .parse::<Address>()
        .expect("canonical public Tribute owner address");
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let (nod_id, mut body) = wait_for_materialized_nod(world, port, owner);
    assert!(
        !body.costAmountMinor.is_zero(),
        "settlement E2E requires a Nod with a nonzero cost"
    );
    assert!(!body.gratisLoadMinor.is_zero());

    if !body.isQualified {
        let qualifying_rate = body
            .floorPriceMinor
            .checked_add(U256::ONE)
            .expect("Nod floor price admits one exact higher scale-6 quote");
        crate::features::price_oracle::publish_controlled_quote(world, qualifying_rate);
        body = wait_for_qualified_materialized_nod(world, port, owner, &nod_id);
    }
    assert!(body.isQualified, "the Nod must be qualified to be mineable");

    let fixture = deploy_settlement_fixture(world);
    // The cost is paid by depositing a note and then spending it. The value
    // reaches the reserve vault at deposit time, so the owner funds and
    // approves the PayNote pool rather than the NodFactory.
    let cost_minor =
        u128::try_from(body.costAmountMinor).expect("Nod cost fits a PayNote spend amount");
    let paynote_proof =
        paynote::deposit_and_prove(world, port, &key, owner, fixture.asset, cost_minor);
    assert_eq!(
        eth::read_call(
            &url,
            fixture.asset,
            &ISettlementAsset::balanceOfCall {
                account: fixture.vault,
            },
        ),
        Some(body.costAmountMinor),
        "reserve vault did not receive exact Nod cost at deposit time"
    );

    let keys = eth::derive_account_keys(&url, &key, Ledger::Gratis)
        .expect("derive public Tribute owner Gratis keys");
    let gratis_before = gratis_balance(&url, owner, &keys.view);
    let mint_nonce = eth::read_call(
        &url,
        addresses::GRATIS_ADDR,
        &eth::IGratis::opNonceOfCall { account: owner },
    )
    .expect("Gratis nonce before Nod mining");
    let chain_id = chain_id_b256(world);
    let mint_mac = outbe_tee_enclave::gratis::modify_mac(
        &keys.modify,
        owner,
        GratisOp::Mint,
        body.gratisLoadMinor,
        mint_nonce,
        chain_id,
    );
    let pow = find_pow_nonce(U256::from_be_slice(&nod_id));
    let mine_gratis = eth::send_call_outcome(
        &url,
        addresses::NOD_FACTORY_ADDR,
        &key,
        &eth::INodFactory::mineGratisCall {
            nodId: U256::from_be_slice(&nod_id),
            nonce: pow,
            mac: B256::from(mint_mac),
            opNonce: mint_nonce,
            payNoteProof: paynote_proof.into(),
        },
        None,
    )
    .expect("mine Gratis by spending the deposited PayNote");
    assert_mined_success(
        &mine_gratis,
        "mine Gratis by spending the deposited PayNote",
    );
    assert_eq!(
        gratis_balance(&url, owner, &keys.view),
        gratis_before + body.gratisLoadMinor,
        "Nod load was not minted exactly into owner Gratis"
    );

    let burn_nonce = eth::read_call(
        &url,
        addresses::GRATIS_ADDR,
        &eth::IGratis::opNonceOfCall { account: owner },
    )
    .expect("Gratis nonce before COEN mining");
    let burn_mac = outbe_tee_enclave::gratis::modify_mac(
        &keys.modify,
        owner,
        GratisOp::Burn,
        body.gratisLoadMinor,
        burn_nonce,
        chain_id,
    );
    let native_before = eth::balance(&url, owner).expect("native balance before Gratis burn");
    let mine_coen = eth::send_call_outcome(
        &url,
        addresses::GRATIS_FACTORY_ADDR,
        &key,
        &eth::IGratisFactory::mineCoenCall {
            amount: body.gratisLoadMinor,
            mac: B256::from(burn_mac),
            opNonce: burn_nonce,
        },
        None,
    )
    .expect("mine COEN from public Tribute owner Gratis");
    assert!(
        mine_coen.success,
        "mine COEN from public Tribute owner Gratis reverted: {}",
        mine_coen.receipt
    );
    let fee =
        crate::world::rpc::Rpc::receipt_gas_cost(&mine_coen.receipt).expect("Gratis burn gas cost");
    let native_after = eth::balance(&url, owner).expect("native balance after Gratis burn");
    assert_eq!(gratis_balance(&url, owner, &keys.view), gratis_before);
    assert_eq!(
        native_after + fee,
        native_before
            + checked_protocol_to_native(body.gratisLoadMinor)
                .expect("Gratis load fits native COEN")
    );
    eprintln!(
        "settlement_evidence kind=nod_to_coen owner={owner:#x} nod_id=0x{} asset={:#x} vault={:#x} cost={} gratis={} tx={} native_before={} native_after={} gas={fee}",
        hex::encode(&nod_id), fixture.asset, fixture.vault, body.costAmountMinor,
        body.gratisLoadMinor, mine_coen.transaction_hash, native_before, native_after
    );
}

fn wait_for_materialized_nod(
    world: &World,
    port: u16,
    owner: Address,
) -> (Vec<u8>, crate::internal::eth::INod::NodData) {
    let deadline = Instant::now() + Duration::from_secs(MATERIALIZED_NOD_TIMEOUT_SECS);
    loop {
        match world.rpc.materialized_nod_for_owner(port, owner) {
            Ok(Some(nod)) => return nod,
            Ok(None) if Instant::now() < deadline => {}
            Err(_) if Instant::now() < deadline => {}
            Ok(None) => panic!(
                "public Tribute owner's materialized Nod did not appear within {MATERIALIZED_NOD_TIMEOUT_SECS}s: owner={owner:#x} head={:?} finalized={:?}",
                world.rpc.head(port),
                world.rpc.finalized(port)
            ),
            Err(error) => panic!(
                "public Tribute owner's materialized Nod remained unreadable after {MATERIALIZED_NOD_TIMEOUT_SECS}s: owner={owner:#x} error={error} head={:?} finalized={:?}",
                world.rpc.head(port),
                world.rpc.finalized(port)
            ),
        }
        sleep(Duration::from_millis(250));
    }
}

fn wait_for_qualified_materialized_nod(
    world: &World,
    port: u16,
    owner: Address,
    expected_nod_id: &[u8],
) -> crate::internal::eth::INod::NodData {
    let deadline = Instant::now() + Duration::from_secs(MATERIALIZED_NOD_TIMEOUT_SECS);
    loop {
        let (candidate, observation) = match world.rpc.materialized_nod_for_owner(port, owner) {
            Ok(Some((nod_id, body))) => {
                let observation = format!(
                    "nod_id=0x{} qualified={} floor={}",
                    hex::encode(&nod_id),
                    body.isQualified,
                    body.floorPriceMinor,
                );
                (Some((nod_id, body)), observation)
            }
            Ok(None) => (None, "Nod not found".to_owned()),
            Err(error) => (None, format!("Nod lookup error: {error}")),
        };
        if let Some((nod_id, body)) = candidate {
            assert_eq!(
                nod_id, expected_nod_id,
                "owner's materialized Nod changed while awaiting qualification"
            );
            if body.isQualified {
                return body;
            }
        }
        assert!(
            Instant::now() < deadline,
            "materialized Nod did not qualify within {MATERIALIZED_NOD_TIMEOUT_SECS}s: owner={owner:#x} {observation} head={:?} finalized={:?}",
            world.rpc.head(port),
            world.rpc.finalized(port),
        );
        sleep(Duration::from_millis(250));
    }
}

pub(crate) fn deploy_settlement_fixture(world: &World) -> SettlementFixture {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let funder = world.validators.get(0);
    let funding = world
        .rpc
        .fund_key(&funder, DEPLOYER_KEY, 1_000)
        .expect("fund settlement fixture deployer");
    assert!(world.rpc.wait_successful_receipt(&funding, 60));

    let intex = environment().repo.join("contracts/intex");
    let asset = address_from(
        &forge::run_with_ctor(
            &intex,
            &[
                "create",
                "test/mocks/MockReferenceStablecoin.sol:MockReferenceStablecoin",
            ],
            &[&USD_ISO.to_string()],
            &[],
            &url,
        )
        .expect("deploy reference stablecoin fixture"),
        "Deployed to:",
    )
    .expect("reference stablecoin address");
    let vault = address_from(
        &forge::run_with_ctor(
            &intex,
            &[
                "create",
                "test/mocks/MockSettlementVault.sol:MockSettlementVault",
            ],
            &[&format!("{asset:#x}")],
            &[],
            &url,
        )
        .expect("deploy settlement vault fixture"),
        "Deployed to:",
    )
    .expect("settlement vault address");

    assert_eq!(
        eth::read_call(&url, asset, &ISettlementAsset::decimalsCall {}),
        Some(6)
    );
    assert_eq!(
        eth::read_call(&url, asset, &ISettlementAsset::isoCodeCall {}),
        Some(USD_ISO)
    );
    assert_eq!(
        eth::read_call(&url, vault, &ISettlementVault::ownerCall {}),
        Some(Address::ZERO)
    );
    assert_eq!(
        eth::read_call(&url, vault, &ISettlementVault::assetCall {}),
        Some(asset)
    );

    let owner_key = funder.evm_key().expect("VaultRouter owner key");
    let add = eth::send_call(
        &url,
        addresses::VAULT_ROUTER_ADDR,
        &owner_key,
        &eth::IVaultRouter::addVaultCall { vault },
        None,
    )
    .expect("register settlement vault");
    assert_success(&url, &add, "register settlement vault");
    assert_eq!(
        eth::read_call(
            &url,
            addresses::VAULT_ROUTER_ADDR,
            &eth::IVaultRouter::referenceCurrencyAssetsCall { isoCode: USD_ISO },
        ),
        Some(vec![asset])
    );
    SettlementFixture { asset, vault }
}

pub(crate) fn fund_and_approve(
    world: &World,
    asset: Address,
    owner_key: &str,
    owner: Address,
    spender: Address,
    amount: U256,
) {
    let url = world.rpc.url(world.validators.primary_port());
    let mint = eth::send_call(
        &url,
        asset,
        DEPLOYER_KEY,
        &ISettlementAsset::mintCall { to: owner, amount },
        None,
    )
    .expect("mint exact settlement amount");
    assert_success(&url, &mint, "mint exact settlement amount");
    let approve = eth::send_call(
        &url,
        asset,
        owner_key,
        &ISettlementAsset::approveCall { spender, amount },
        None,
    )
    .expect("approve settlement factory");
    assert_success(&url, &approve, "approve settlement factory");
}

fn wait_for_validator_reward_gem(world: &World) -> (Address, U256, eth::IGem::GemData) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let key = world.validators.get(0).evm_key().expect("validator 0 key");
    let owner = world
        .rpc
        .address_of(&key)
        .expect("validator 0 address")
        .parse::<Address>()
        .expect("canonical validator 0 address");
    let deadline = Instant::now() + Duration::from_secs(REWARD_GEM_TIMEOUT_SECS);
    loop {
        if let Some(balance) = eth::read_call(
            &url,
            addresses::GEM_ADDR,
            &eth::IGem::balanceOfCall { owner },
        ) {
            if !balance.is_zero() {
                let index = balance - U256::from(1);
                let gem_id = eth::read_call(
                    &url,
                    addresses::GEM_ADDR,
                    &eth::IGem::tokenOfOwnerByIndexCall { owner, index },
                )
                .expect("validator reward Gem enumeration");
                let gem = eth::read_call(
                    &url,
                    addresses::GEM_ADDR,
                    &eth::IGem::getGemStatusCall { gemId: gem_id },
                )
                .expect("validator reward Gem data");
                return (owner, gem_id, gem);
            }
        }
        assert!(
            Instant::now() < deadline,
            "validator 0 did not receive a protocol reward Gem; head={:?} finalized={:?}",
            world.rpc.head(port),
            world.rpc.finalized(port)
        );
        sleep(Duration::from_millis(500));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RewardGemQueueSnapshot {
    head: u64,
    tail: u64,
    pending_batch_count: u64,
    reward_utc_day: Option<u32>,
    recipient_count: u32,
    is_prepared: bool,
    is_settled: bool,
}

fn validator_reward_owner(world: &World) -> Address {
    let key = world.validators.get(0).evm_key().expect("validator 0 key");
    world
        .rpc
        .address_of(&key)
        .expect("validator 0 address")
        .parse()
        .expect("canonical validator 0 address")
}

fn validator_reward_gem_balance(world: &World, port: u16) -> U256 {
    eth::read_call(
        &world.rpc.url(port),
        addresses::GEM_ADDR,
        &eth::IGem::balanceOfCall {
            owner: validator_reward_owner(world),
        },
    )
    .expect("validator reward Gem balance")
}

fn reward_gem_id_at(world: &World, port: u16, index: U256) -> U256 {
    eth::read_call(
        &world.rpc.url(port),
        addresses::GEM_ADDR,
        &eth::IGem::tokenOfOwnerByIndexCall {
            owner: validator_reward_owner(world),
            index,
        },
    )
    .expect("validator reward Gem enumeration")
}

fn reward_gem_queue_snapshot(world: &World, port: u16) -> Option<RewardGemQueueSnapshot> {
    let url = world.rpc.url(port);
    let rewards = outbe_primitives::addresses::REWARDS_ADDRESS;
    let read = |slot: U256| eth::storage(&url, rewards, slot);
    let head = u64::try_from(read(U256::from(REWARD_GEM_QUEUE_HEAD_SLOT))?).ok()?;
    let tail = u64::try_from(read(U256::from(REWARD_GEM_QUEUE_TAIL_SLOT))?).ok()?;
    let pending_batch_count =
        u64::try_from(read(U256::from(REWARD_GEM_PENDING_BATCH_COUNT_SLOT))?).ok()?;
    if head > tail || pending_batch_count != tail - head {
        return None;
    }
    if head == tail {
        return Some(RewardGemQueueSnapshot {
            head,
            tail,
            pending_batch_count,
            reward_utc_day: None,
            recipient_count: 0,
            is_prepared: false,
            is_settled: false,
        });
    }
    let reward_utc_day = u32::try_from(read(
        U256::from(head).mapping_slot(U256::from(REWARD_GEM_UTC_DAY_BY_SEQUENCE_SLOT)),
    )?)
    .ok()?;
    let day_key = U256::from(reward_utc_day);
    let recipient_count = u32::try_from(read(
        day_key.mapping_slot(U256::from(REWARD_GEM_RECIPIENT_COUNT_SLOT)),
    )?)
    .ok()?;
    let is_prepared = !read(day_key.mapping_slot(U256::from(DAILY_TOPUP_PREPARED_SLOT)))?.is_zero();
    let is_settled = !read(day_key.mapping_slot(U256::from(DAILY_TOPUP_SETTLED_SLOT)))?.is_zero();
    Some(RewardGemQueueSnapshot {
        head,
        tail,
        pending_batch_count,
        reward_utc_day: Some(reward_utc_day),
        recipient_count,
        is_prepared,
        is_settled,
    })
}

fn assert_reward_gem_queue_parity(world: &World, expected: RewardGemQueueSnapshot) {
    for port in world.validators.committee_ports() {
        assert_eq!(
            reward_gem_queue_snapshot(world, port),
            Some(expected),
            "validator {port} observes a different Rewards FIFO"
        );
    }
}

fn find_canonical_reward_gem_delivery_block_number(world: &World, gem_id: U256) -> Option<u64> {
    canonical_reward_gem_delivery_block_numbers(world, gem_id)
        .into_iter()
        .next()
}

fn canonical_reward_gem_delivery_block_numbers(world: &World, gem_id: U256) -> Vec<u64> {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let Some(finalized) = world.rpc.finalized(port) else {
        return Vec::new();
    };
    let from = finalized.saturating_sub(MAX_REWARD_DELIVERY_SCAN_BLOCKS.saturating_sub(1));
    let Some(blocks) = eth::blocks_with_transactions(
        &url,
        from,
        finalized,
        usize::try_from(MAX_REWARD_DELIVERY_SCAN_BLOCKS).unwrap_or(usize::MAX),
    ) else {
        return Vec::new();
    };
    let gem_id_topic = format!("{:#066x}", gem_id);
    let gem_issued_topic = format!("{:#x}", eth::IGemFactory::GemIssued::SIGNATURE_HASH);
    let delivery_prefix = "0x4f53473202";
    let cycle_prefix = "0x4f53433202";
    let mut matching_block_numbers = Vec::new();
    for (block_number, block) in (from..=finalized).zip(blocks) {
        let Some(transactions) = block
            .get("transactions")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for (index, transaction) in transactions.iter().enumerate() {
            if index == 0 {
                continue;
            }
            let Some(transaction_hash) =
                transaction.get("hash").and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let Some(receipt) = eth::receipt_json(&url, transaction_hash) else {
                continue;
            };
            if transaction_is_reward_delivery_for_gem(
                &transactions[index - 1],
                transaction,
                &receipt,
                cycle_prefix,
                delivery_prefix,
                &gem_issued_topic,
                &gem_id_topic,
            ) {
                matching_block_numbers.push(block_number);
            }
        }
    }
    matching_block_numbers
}

fn transaction_is_reward_delivery_for_gem(
    previous: &serde_json::Value,
    transaction: &serde_json::Value,
    receipt: &serde_json::Value,
    cycle_prefix: &str,
    delivery_prefix: &str,
    gem_issued_topic: &str,
    gem_id_topic: &str,
) -> bool {
    let input = transaction
        .get("input")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let previous_input = previous
        .get("input")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let to = transaction
        .get("to")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<Address>().ok());
    let success = receipt
        .get("status")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|status| status == "0x1");
    input.starts_with(delivery_prefix)
        && previous_input.starts_with(cycle_prefix)
        && to == Some(outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS)
        && success
        && receipt
            .get("logs")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|logs| {
                logs.iter().any(|log| {
                    let address_matches = log
                        .get("address")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|value| value.parse::<Address>().ok())
                        == Some(addresses::GEM_FACTORY_ADDR);
                    let topics = log.get("topics").and_then(serde_json::Value::as_array);
                    address_matches
                        && topics.is_some_and(|topics| {
                            topics
                                .first()
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|topic| topic.eq_ignore_ascii_case(gem_issued_topic))
                                && topics
                                    .get(1)
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(|topic| topic.eq_ignore_ascii_case(gem_id_topic))
                        })
                })
            })
}

fn chain_id_b256(world: &World) -> B256 {
    B256::from(U256::from(
        world
            .rpc
            .chain_id(world.validators.primary_port())
            .expect("settlement chain ID"),
    ))
}

fn find_pow_nonce(id: U256) -> u64 {
    (0_u64..100_000)
        .find(|nonce| outbe_common::pow::validate_pow(id, *nonce).is_ok())
        .expect("bounded PoW nonce")
}

fn promis_balance(url: &str, owner: Address, view_key: &[u8; 32]) -> U256 {
    let blob = eth::read_call(
        url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::balanceOfCall { account: owner },
    )
    .expect("Promis ciphertext");
    if blob.is_empty() {
        U256::ZERO
    } else {
        outbe_tee_enclave::promis::decrypt_balance(view_key, owner, blob.as_ref())
            .expect("decrypt Promis balance")
    }
}

fn gratis_balance(url: &str, owner: Address, view_key: &[u8; 32]) -> U256 {
    let blob = eth::read_call(
        url,
        addresses::GRATIS_ADDR,
        &eth::IGratis::balanceOfCall { account: owner },
    )
    .expect("Gratis ciphertext");
    if blob.is_empty() {
        U256::ZERO
    } else {
        outbe_tee_enclave::gratis::decrypt_balance(view_key, owner, blob.as_ref())
            .expect("decrypt Gratis balance")
    }
}

fn assert_success(url: &str, tx: &str, label: &str) {
    let _ = successful_receipt(url, tx, label);
}

pub(crate) fn assert_mined_success(outcome: &eth::MinedCallOutcome, label: &str) {
    assert!(outcome.success, "{label} reverted: {}", outcome.receipt);
}

fn successful_receipt(url: &str, tx: &str, label: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(receipt) = eth::receipt_json(url, tx) {
            assert_eq!(
                receipt.get("status").and_then(serde_json::Value::as_str),
                Some("0x1"),
                "{label} reverted: {receipt}"
            );
            return receipt;
        }
        assert!(Instant::now() < deadline, "{label} receipt timed out: {tx}");
        sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivery_fixture(gem_id: U256) -> (serde_json::Value, serde_json::Value, serde_json::Value) {
        let topic = format!("{:#x}", eth::IGemFactory::GemIssued::SIGNATURE_HASH);
        let gem_id = format!("{gem_id:#066x}");
        (
            serde_json::json!({ "input": "0x4f53433202" }),
            serde_json::json!({
                "input": "0x4f53473202",
                "to": format!("{:#x}", outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS),
            }),
            serde_json::json!({
                "status": "0x1",
                "logs": [{
                    "address": format!("{:#x}", addresses::GEM_FACTORY_ADDR),
                    "topics": [topic, gem_id],
                }],
            }),
        )
    }

    #[test]
    fn delivery_evidence_requires_cycle_then_osg2_and_the_exact_gem_event() {
        let gem_id = U256::from(7);
        let (cycle, delivery, receipt) = delivery_fixture(gem_id);
        let topic = format!("{:#x}", eth::IGemFactory::GemIssued::SIGNATURE_HASH);
        let gem_id_topic = format!("{gem_id:#066x}");
        assert!(transaction_is_reward_delivery_for_gem(
            &cycle,
            &delivery,
            &receipt,
            "0x4f53433202",
            "0x4f53473202",
            &topic,
            &gem_id_topic,
        ));

        let wrong_predecessor = serde_json::json!({ "input": "0x4f53413202" });
        assert!(!transaction_is_reward_delivery_for_gem(
            &wrong_predecessor,
            &delivery,
            &receipt,
            "0x4f53433202",
            "0x4f53473202",
            &topic,
            &gem_id_topic,
        ));
        assert!(!transaction_is_reward_delivery_for_gem(
            &cycle,
            &delivery,
            &receipt,
            "0x4f53433202",
            "0x4f53473202",
            &topic,
            &format!("{:#066x}", U256::from(8)),
        ));
    }
}
