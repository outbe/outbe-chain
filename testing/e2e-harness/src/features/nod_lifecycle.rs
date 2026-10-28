//! What a Nod supplies to the shared lifecycle. One group holds five Nods. Each owner
//! submits one encrypted Tribute, Metadosis Lysis seals it, and the owner mints through
//! `ITribute::createNod`. The harness then moves the group stamp. Two are paid while
//! issued, two are inside the call notice, and one is forfeited.

use outbe_primitives::time::SECONDS_PER_DAY;
use std::time::Duration;

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use cucumber::{given, when};
use outbe_nod::config::NodParams;

use crate::features::entity_lifecycle::chain::{
    assert_single_event, finalized_checkpoint, head_time, poll_until,
};
use crate::features::entity_lifecycle::entity::{Item, Lifecycle, Phase, Target, Terms};
use crate::features::entity_lifecycle::holders::{self, FORFEITED, HOLDERS, UNPAID_AT_CALL};
use crate::features::entity_lifecycle::markets::MYR_ISO;
use crate::features::entity_lifecycle::phases::CALL_WINDOW_SEED_DAYS;
use crate::features::entity_lifecycle::redeem::{self, mint_authorization, Ledger, Mined};
use crate::features::settlement::{assert_mined_success, find_mining_pow_nonce};
use crate::internal::{addresses, eth};
use crate::world::forge::DEPLOYER_KEY;
use crate::world::rpc::TributeOfferParams;
use crate::world::settlement_currency::USD_ISO;
use crate::world::World;

alloy_sol_types::sol! {
    interface INodFactoryTestArming {
        function backdateNodForTest(uint256 nodId, uint64 issuedAt, uint256 gratisLoadMinor) external;
        function closeCallNoticeForTest(bytes32 groupKey, uint64 deadline) external;
    }
}

/// A Nod's id derives from its owner and day, so one group needs an owner per Nod.
const HOLDER_SEED: u64 = 0x0e0d_0000;
/// Shared with Gem and Intex, and low enough that the controlled COEN/USD quote clears
/// the DEV call price.
const ENTRY_PRICE_MINOR: u64 = 800_000;
/// `effectiveState` of `INod.NodData`.
const ISSUED: u8 = 0;
const CALLED: u8 = 1;
const SETTLED: u8 = 2;
/// After the notice lapses, the next sweep burns the unpaid Nod.
const FORFEIT_TIMEOUT: Duration = Duration::from_secs(300);
/// A DEV notice is minutes long. Anything longer means the profile is not active.
const MAX_DEV_NOTICE_SECS: u32 = 3_600;
/// Slack past the deadline so the sweep has a block to run in.
const NOTICE_MARGIN_SECS: u64 = 5;
/// Forfeits become eligible after the UTC day containing the deadline closes.
const EXPIRY_DAY_SECS: u64 = 86_400;

pub(crate) struct NodLifecycle;

#[given(expr = "a fresh Nod lifecycle localnet with a {int}-block voting window")]
fn fresh_nod_lifecycle_localnet(world: &mut World, window: u64) {
    super::common::bootstrap_localnet(
        world,
        window,
        &[("TESTNET_METADOSIS_OFFERING_SECONDS", "3600".to_owned())],
    );
    let day = world
        .localnet
        .prepare_nod_lifecycle_fixture(ENTRY_PRICE_MINOR)
        .expect("Nod lifecycle Offering and Lysis fixture");
    world.state.wwd = Some(day.to_string());
    super::common::start_bootstrapped_localnet(
        world,
        &crate::world::localnet::StartOpts::with_voting_window(window),
    );
}

#[when("five owners are issued Nods in one group")]
fn issue_five_nods(world: &mut World) {
    let issuance_currency = world
        .state
        .issuance_market
        .expect("the Nod scenario prices an issuance market");
    assert_eq!(issuance_currency, MYR_ISO);
    holders::fund(world, HOLDER_SEED);
    let keys: Vec<String> = (0..HOLDERS).map(owner_key).collect();
    super::l2_registration::ensure_tribute_offer_operators(world, &keys);
    let wwd = world.state.wwd.clone().expect("Nod offering day");
    super::tribute_projection::wait_for_offering(world, &wwd);
    for key in &keys {
        let tx_hash = world
            .rpc
            .tribute_cross_currency_offer(
                key,
                TributeOfferParams {
                    wwd: &wwd,
                    amounts: ("1", "0"),
                    currency: issuance_currency,
                },
                USD_ISO,
            )
            .expect("submit encrypted Tribute");
        assert!(
            world.rpc.wait_successful_receipt(&tx_hash, 240),
            "encrypted Tribute did not mine"
        );
    }
    super::lysis::reach_native_lysis_processing_time(world);
    super::lysis::wait_for_native_lysis(world);

    let url = world.rpc.url(world.validators.primary_port());
    let day = wwd.parse::<u32>().expect("numeric offering day");
    let worldwide_day = outbe_primitives::time::WorldwideDay::new(day);
    // The call counts only days after the group's stamp: stamp it behind every day
    // the scenario seeds.
    let issued_at =
        head_time(world).saturating_sub((u64::from(CALL_WINDOW_SEED_DAYS) + 1) * SECONDS_PER_DAY);
    let mut nods = Vec::with_capacity(HOLDERS);
    let mut shared_load = None;
    for (index, key) in keys.iter().enumerate().take(HOLDERS) {
        let owner = owner_address(index);
        let tribute_id =
            outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(owner, worldwide_day)
                .expect("tribute id")
                .to_u256();
        let outcome = eth::send_call_outcome(
            &url,
            addresses::TRIBUTE_ADDR,
            key,
            &eth::ITribute::createNodCall {
                tributeId: tribute_id,
            },
            Some(U256::ZERO),
        )
        .expect("submit ITribute::createNod");
        assert_mined_success(&outcome, "ITribute::createNod");
        let nod_id = nod_of(&url, owner);
        let load = sealed_load(world, nod_id);
        match shared_load {
            None => shared_load = Some(load),
            Some(expected) => assert_eq!(load, expected, "the five Nods must share one load"),
        }
        let outcome = eth::send_call_outcome(
            &url,
            addresses::NOD_FACTORY_ADDR,
            DEPLOYER_KEY,
            &INodFactoryTestArming::backdateNodForTestCall {
                nodId: nod_id,
                issuedAt: issued_at,
                gratisLoadMinor: load,
            },
            None,
        )
        .expect("submit backdateNodForTest");
        assert_mined_success(&outcome, "backdateNodForTest");
        nods.push(nod_id);
    }
    world.state.lifecycle_nods = nods;
}

impl Lifecycle for NodLifecycle {
    fn call_price(&self, world: &World) -> U256 {
        read_nod(world, nod(world, 0)).callPriceMinor
    }

    fn terms(&self, world: &World, item: &Item) -> Terms {
        let Item::Nod(id) = item else {
            unreachable!("a Nod scenario pays only for Nods")
        };
        let data = read_nod(world, *id);
        Terms {
            entry_price: data.entryPriceMinor,
            load: crate::internal::nod_keys::decrypt(world, &data),
        }
    }

    fn settlement_terms(&self, world: &World, target: &Target, phase: Phase) -> Terms {
        let Item::Nod(id) = target.item else {
            unreachable!("a Nod scenario pays only for Nods")
        };
        let expected_state = match phase {
            Phase::Issued => ISSUED,
            Phase::Called => CALLED,
        };
        let data = read_nod(world, id);
        assert!(
            data.owner == target.owner && !data.isSettled && data.effectiveState == expected_state,
            "Nod {id} is not ready for {phase:?} settlement: owner={}, settled={}, state={}",
            data.owner,
            data.isSettled,
            data.effectiveState
        );
        Terms {
            entry_price: data.entryPriceMinor,
            load: crate::internal::nod_keys::decrypt(world, &data),
        }
    }

    fn assert_issued(&self, world: &World) {
        let entry = U256::from(ENTRY_PRICE_MINOR);
        let issuance_currency = world.state.issuance_market.expect("issuance market");
        let first = read_nod(world, nod(world, 0));
        for index in 0..HOLDERS {
            let data = read_nod(world, nod(world, index));
            assert_eq!(data.owner, owner_address(index));
            assert_eq!(data.effectiveState, ISSUED);
            assert!(!data.isSettled);
            assert_eq!(data.calledAt, 0);
            assert_eq!(data.settlementDeadline, 0);
            assert_eq!(
                (data.issuanceCurrency, data.referenceCurrency),
                (issuance_currency, USD_ISO)
            );
            assert_eq!(data.entryPriceMinor, entry);
            let load = crate::internal::nod_keys::decrypt(world, &data);
            assert!(!load.is_zero(), "Lysis sealed a zero Nod load");
            assert_eq!(
                load,
                crate::internal::nod_keys::decrypt(world, &first),
                "the five Nods must share one load"
            );
            assert_eq!(
                data.settlementCostMinor,
                entry * load / U256::from(1_000_000)
            );
            let dev = NodParams::DEV;
            assert_eq!(
                data.callPriceMinor,
                entry * U256::from(100 + dev.call_rate) / U256::from(100)
            );
            assert_eq!(
                (data.callRate, data.callWindow, data.callThreshold),
                (
                    dev.call_rate,
                    dev.call_window_seconds,
                    dev.call_threshold_seconds
                ),
                "the group did not seal the DEV call terms"
            );
            assert!(
                data.callNoticePeriod <= MAX_DEV_NOTICE_SECS,
                "call notice is {}s: the DEV profile is not active",
                data.callNoticePeriod
            );
            assert_eq!(
                (data.worldwideDay, data.entryPriceMinor),
                (first.worldwideDay, first.entryPriceMinor),
                "every Nod must share one group"
            );
        }
    }

    fn targets(&self, world: &World, phase: Phase) -> [Target; 2] {
        holders::paid_in(phase).map(|index| target(world, index))
    }

    fn called(&self, world: &World) -> bool {
        let unpaid = UNPAID_AT_CALL.map(|index| read_nod(world, nod(world, index)));
        let called_at = unpaid[0].calledAt;
        unpaid.iter().all(|data| {
            data.effectiveState == CALLED
                && data.calledAt == called_at
                && data.settlementDeadline == called_at + u64::from(data.callNoticePeriod)
        }) && head_time(world) <= unpaid[0].settlementDeadline
    }

    fn assert_paid_settled(&self, world: &World) {
        for paid in &world.state.entity_lifecycle.payments {
            let Item::Nod(id) = paid.target.item else {
                unreachable!("a Nod scenario pays only for Nods")
            };
            let data = read_nod(world, id);
            assert!(data.isSettled, "paid Nod {id} is not settled");
            assert_eq!(data.effectiveState, SETTLED, "paid Nod {id} left Settled");
        }
    }

    fn lapse_notice(&self, world: &mut World) {
        let port = world.validators.primary_port();
        let url = world.rpc.url(port);
        let height = eth::block_number(&url).expect("head before the notice lapses");
        let pool = eth::read_call_at(
            &url,
            addresses::PROMIS_LIMIT_ADDR,
            &eth::IPromisLimit::promisLimitCall {},
            height,
        )
        .expect("unallocated pool before the forfeit");
        world.state.entity_lifecycle.pool_before_forfeit = Some((height, pool));
        let id = nod(world, FORFEITED);
        let called = read_nod(world, id);
        world.state.entity_lifecycle.nod_before_forfeit = Some((height, id, called.groupKey));
        let deadline = called.settlementDeadline + NOTICE_MARGIN_SECS;
        poll_until(
            Duration::from_secs(u64::from(called.callNoticePeriod)) + FORFEIT_TIMEOUT,
            || format!("the chain never passed the Nod settlement deadline {deadline}"),
            || head_time(world) > deadline,
        );
        eth::send_call(
            &url,
            addresses::NOD_FACTORY_ADDR,
            DEPLOYER_KEY,
            &INodFactoryTestArming::closeCallNoticeForTestCall {
                groupKey: called.groupKey,
                deadline: head_time(world).saturating_sub(EXPIRY_DAY_SECS),
            },
            None,
        )
        .expect("arm the Nod group's closed deadline day");
    }

    fn assert_forfeited(&self, world: &World) {
        let url = world.rpc.url(world.validators.primary_port());
        let owner = owner_address(FORFEITED);
        let id = nod(world, FORFEITED);
        let load = forfeited_load(world, id);
        assert_burned(world, owner, id, FORFEIT_TIMEOUT);
        let (from, before) = world
            .state
            .entity_lifecycle
            .pool_before_forfeit
            .expect("the pool was read before the notice lapsed");
        let height = finalized_checkpoint(world).height;
        let (captured_height, captured_id, group) = world
            .state
            .entity_lifecycle
            .nod_before_forfeit
            .expect("the Nod group captured before the notice lapsed");
        assert_eq!((captured_height, captured_id), (from, id));
        assert_single_event(
            &url,
            addresses::NOD_ADDR,
            from,
            height,
            eth::INod::NodGroupForfeited {
                groupKey: group,
                nods: 1,
                gratisLoadMinor: load,
            },
        );
        assert_eq!(
            eth::read_call_at(
                &url,
                addresses::PROMIS_LIMIT_ADDR,
                &eth::IPromisLimit::promisLimitCall {},
                height,
            ),
            Some(before + load),
            "the forfeit did not return exactly the Nod's load to the unallocated pool"
        );
    }

    fn mine_paid(&self, world: &World) -> Vec<Mined> {
        let url = world.rpc.url(world.validators.primary_port());
        let load = sealed_load(world, nod(world, 0));
        holders::paid()
            .map(|index| {
                let key = owner_key(index);
                let owner = owner_address(index);
                let id = nod(world, index);
                let before = redeem::balance(world, Ledger::Gratis, &key, owner);
                let (mac, op_nonce) = mint_authorization(world, Ledger::Gratis, &key, owner, load);
                let outcome = eth::send_call_outcome(
                    &url,
                    addresses::NOD_FACTORY_ADDR,
                    &key,
                    &paid_nod_mining_call(id, owner, mac, op_nonce),
                    None,
                )
                .expect("submit Gratis mining");
                assert_mined_success(&outcome, "mine Gratis from the paid Nod");
                assert_burned(world, owner, id, Duration::ZERO);
                Mined {
                    owner,
                    owner_key: key,
                    ledger: Ledger::Gratis,
                    before,
                    amount: load,
                }
            })
            .collect()
    }
}

fn owner_key(index: usize) -> String {
    holders::key(HOLDER_SEED, index)
}

fn owner_address(index: usize) -> Address {
    holders::address(HOLDER_SEED, index)
}

fn nod(world: &World, index: usize) -> U256 {
    world.state.lifecycle_nods[index]
}

fn sealed_load(world: &World, id: U256) -> U256 {
    let load = crate::internal::nod_keys::decrypt(world, &read_nod(world, id));
    assert!(!load.is_zero(), "Lysis sealed a zero Nod load");
    load
}

/// The forfeited Nod's load, read before the sweep deletes its body.
/// A block between the arming transaction and this step may already have burned it;
/// the other four Nods carry the same load.
fn forfeited_load(world: &World, id: U256) -> U256 {
    let url = world.rpc.url(world.validators.primary_port());
    let load = match eth::read_call(
        &url,
        addresses::NOD_ADDR,
        &eth::INod::getNodCall { nodId: id },
    ) {
        Some(data) => crate::internal::nod_keys::decrypt(world, &data),
        None => sealed_load(world, nod(world, 0)),
    };
    assert!(!load.is_zero(), "Lysis sealed a zero Nod load");
    load
}

fn target(world: &World, index: usize) -> Target {
    Target {
        item: Item::Nod(nod(world, index)),
        owner: owner_address(index),
        owner_key: owner_key(index),
        issuance_currency: world.state.issuance_market.expect("issuance market"),
    }
}

/// The Nod is gone: its owner holds none, and its data no longer reads.
fn assert_burned(world: &World, owner: Address, id: U256, timeout: Duration) {
    poll_until(
        timeout,
        || format!("Nod {id} was not burned"),
        || nod_balance(world, owner).is_zero(),
    );
    assert!(
        eth::read_call(
            &world.rpc.url(world.validators.primary_port()),
            addresses::NOD_ADDR,
            &eth::INod::getNodCall { nodId: id }
        )
        .is_none(),
        "a burned Nod must not be readable"
    );
}

fn nod_balance(world: &World, owner: Address) -> U256 {
    read_nod_view(world, &eth::INod::balanceOfCall { owner }, "Nod balance")
}

fn read_nod(world: &World, id: U256) -> eth::INod::NodData {
    read_nod_view(world, &eth::INod::getNodCall { nodId: id }, "Nod data")
}

fn read_nod_view<C>(world: &World, call: &C, what: &str) -> C::Return
where
    C: SolCall,
    C::Return: Send + 'static,
{
    let url = world.rpc.url(world.validators.primary_port());
    eth::read_call_result(&url, addresses::NOD_ADDR, call)
        .unwrap_or_else(|error| panic!("{what}: {error}"))
}

fn nod_of(url: &str, owner: Address) -> U256 {
    eth::read_call_result(
        url,
        addresses::NOD_ADDR,
        &eth::INod::tokenOfOwnerByIndexCall {
            owner,
            index: U256::ZERO,
        },
    )
    .unwrap_or_else(|error| {
        panic!("the Nod issued to {owner} is not in its owner's index: {error}")
    })
}

fn paid_nod_mining_call(
    id: U256,
    owner: Address,
    mac: alloy_primitives::B256,
    op_nonce: u64,
) -> eth::INodFactory::mineGratisCall {
    eth::INodFactory::mineGratisCall {
        nodId: id,
        nonce: find_mining_pow_nonce(outbe_common::pow::MiningDomain::Nod, id, owner),
        mac,
        opNonce: op_nonce,
    }
}
