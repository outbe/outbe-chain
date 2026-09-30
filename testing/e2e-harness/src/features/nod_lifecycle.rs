//! What a Nod supplies to the shared lifecycle: five Nods in one bucket, issued straight
//! to five owners, two paid while qualified, two inside the call notice, one forfeited.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use cucumber::when;
use outbe_nod::constants::{
    CALL_LOOKBACK_DAYS, CALL_NOTICE_PERIOD, CALL_RATE_PCT, CALL_THRESHOLD, CALL_WINDOW,
    FLOOR_RATE_PCT,
};

use crate::features::entity_lifecycle::chain::{
    assert_single_event, finalized_checkpoint, head_time, poll_until,
};
use crate::features::entity_lifecycle::entity::{Item, Lifecycle, Phase, Target, Terms};
use crate::features::entity_lifecycle::holders::{self, FORFEITED, HOLDERS, UNPAID_AT_CALL};
use crate::features::entity_lifecycle::redeem::{self, mint_authorization, Ledger, Mined};
use crate::features::settlement::{assert_mined_success, find_mining_pow_nonce};
use crate::internal::{addresses, eth};
use crate::world::forge::DEPLOYER_KEY;
use crate::world::settlement_currency::USD_ISO;
use crate::world::World;

alloy_sol_types::sol! {
    interface INodFactoryTestArming {
        function issueForTest(
            address owner,
            uint32 worldwideDay,
            uint256 gratisLoadMinor,
            uint256 entryPriceMinor,
            uint16 issuanceCurrency,
            uint16 referenceCurrency,
            uint64 issuedAt
        ) external;
        function closeCallNoticeForTest(uint256 nodId, uint64 deadline) external;
    }
}

/// A Nod's id derives from its owner and day, so one bucket needs an owner per Nod.
const HOLDER_SEED: u64 = 0x0e0d_0000;
/// Low enough that the controlled COEN/USD quote clears even the call price.
const ENTRY_PRICE_MINOR: u64 = 250_000;
/// Not a whole unit, so the cost leaves a remainder to floor.
const GRATIS_LOAD_MINOR: u64 = 5_000_003;
/// `effectiveState` of `INod.NodData`.
const ISSUED: u8 = 0;
const QUALIFIED: u8 = 1;
const CALLED: u8 = 2;
const SETTLED: u8 = 3;
/// Issuance goes through the compressed-body projection before the index shows it.
const ISSUANCE_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a Nod view may trail the block that wrote it.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Once the notice has lapsed the next sweep burns the unpaid Nod.
const FORFEIT_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) struct NodLifecycle;

#[when("five owners are issued Nods in one bucket")]
fn issue_five_nods(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let issuance_currency = world
        .state
        .issuance_market
        .expect("the Nod scenario prices an issuance market");
    let now = head_time(world);
    let day = outbe_primitives::time::worldwide_day_from_timestamp(now);
    // The bucket counts breach days only after it was issued: stamp it behind the
    // whole call window the scenario is about to seed.
    let issued_at = now.saturating_sub((u64::from(CALL_LOOKBACK_DAYS) + 2) * 86_400);
    holders::fund(world, HOLDER_SEED);
    let mut nods = Vec::with_capacity(HOLDERS);
    for index in 0..HOLDERS {
        let owner = owner_address(index);
        let issue = INodFactoryTestArming::issueForTestCall {
            owner,
            worldwideDay: day,
            gratisLoadMinor: U256::from(GRATIS_LOAD_MINOR),
            entryPriceMinor: U256::from(ENTRY_PRICE_MINOR),
            issuanceCurrency: issuance_currency,
            referenceCurrency: USD_ISO,
            issuedAt: issued_at,
        };
        let outcome = eth::send_call_outcome(
            &url,
            addresses::NOD_FACTORY_ADDR,
            DEPLOYER_KEY,
            &issue,
            None,
        )
        .expect("submit test Nod issuance");
        assert_mined_success(&outcome, "test Nod issuance");
        nods.push(wait_for_nod_of(&url, owner));
    }
    world.state.lifecycle_nods = nods;
}

impl Lifecycle for NodLifecycle {
    fn floor(&self, world: &World) -> U256 {
        read_nod(world, nod(world, 0)).floorPriceMinor
    }

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
            load: data.gratisLoadMinor,
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
            assert!(!data.isQualified);
            assert!(!data.isSettled);
            assert_eq!(data.calledAt, 0);
            assert_eq!(data.settlementDeadline, 0);
            assert_eq!(
                (data.issuanceCurrency, data.referenceCurrency),
                (issuance_currency, USD_ISO)
            );
            assert_eq!(data.entryPriceMinor, entry);
            assert_eq!(data.gratisLoadMinor, U256::from(GRATIS_LOAD_MINOR));
            assert_eq!(
                data.settlementCostMinor,
                entry * U256::from(GRATIS_LOAD_MINOR) / U256::from(1_000_000)
            );
            assert_eq!(
                data.floorPriceMinor,
                entry * U256::from(100 + FLOOR_RATE_PCT) / U256::from(100)
            );
            assert_eq!(
                data.callPriceMinor,
                entry * U256::from(100 + CALL_RATE_PCT) / U256::from(100)
            );
            assert_eq!(data.callRate, CALL_RATE_PCT);
            assert_eq!(data.callWindow, CALL_WINDOW);
            assert_eq!(data.callThreshold, CALL_THRESHOLD);
            assert_eq!(data.callNoticePeriod, CALL_NOTICE_PERIOD);
            assert_eq!(
                (data.worldwideDay, data.floorPriceMinor),
                (first.worldwideDay, first.floorPriceMinor),
                "every Nod must share one bucket"
            );
        }
    }

    fn qualified(&self, world: &World) -> bool {
        (0..HOLDERS).all(|index| {
            let data = read_nod(world, nod(world, index));
            data.effectiveState == QUALIFIED && data.isQualified
        })
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
                && data.settlementDeadline == called_at + u64::from(CALL_NOTICE_PERIOD)
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
            &eth::IPromisLimit::totalUnallocatedCall {},
            height,
        )
        .expect("unallocated pool before the forfeit");
        world.state.entity_lifecycle.pool_before_forfeit = Some((height, pool));
        // Seven days of notice have no DEV profile to shorten them: move the bucket's call
        // back so its deadline has just passed.
        let close = INodFactoryTestArming::closeCallNoticeForTestCall {
            nodId: nod(world, FORFEITED),
            deadline: head_time(world) - 1,
        };
        let outcome = eth::send_call_outcome(
            &url,
            addresses::NOD_FACTORY_ADDR,
            DEPLOYER_KEY,
            &close,
            None,
        )
        .expect("submit call notice close");
        assert_mined_success(&outcome, "close the Nod call notice");
    }

    fn assert_forfeited(&self, world: &World) {
        let url = world.rpc.url(world.validators.primary_port());
        let owner = owner_address(FORFEITED);
        let id = nod(world, FORFEITED);
        assert_burned(world, owner, id, FORFEIT_TIMEOUT);
        let (from, before) = world
            .state
            .entity_lifecycle
            .pool_before_forfeit
            .expect("the pool was read before the notice lapsed");
        let height = finalized_checkpoint(world).height;
        let load = U256::from(GRATIS_LOAD_MINOR);
        assert_single_event(
            &url,
            addresses::NOD_ADDR,
            from,
            height,
            eth::INod::NodForfeited {
                owner,
                nodId: id,
                gratisLoadMinor: load,
            },
        );
        assert_eq!(
            eth::read_call_at(
                &url,
                addresses::PROMIS_LIMIT_ADDR,
                &eth::IPromisLimit::totalUnallocatedCall {},
                height,
            ),
            Some(before + load),
            "the forfeit did not return exactly the Nod's load to the unallocated pool"
        );
    }

    fn mine_paid(&self, world: &World) -> Vec<Mined> {
        let url = world.rpc.url(world.validators.primary_port());
        let load = U256::from(GRATIS_LOAD_MINOR);
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
                    &eth::INodFactory::mineGratisCall {
                        nodId: id,
                        nonce: find_mining_pow_nonce(
                            outbe_common::pow::MiningDomain::Nod,
                            id,
                            owner,
                        ),
                        mac,
                        opNonce: op_nonce,
                    },
                    None,
                )
                .expect("submit Gratis mining");
                assert_mined_success(&outcome, "mine Gratis from the paid Nod");
                assert_burned(world, owner, id, READ_TIMEOUT);
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
            &eth::INod::nodDataCall { nodId: id }
        )
        .is_none(),
        "a burned Nod must not be readable"
    );
}

fn nod_balance(world: &World, owner: Address) -> U256 {
    read_nod_view(world, &eth::INod::balanceOfCall { owner }, "Nod balance")
}

fn read_nod(world: &World, id: U256) -> eth::INod::NodData {
    read_nod_view(world, &eth::INod::nodDataCall { nodId: id }, "Nod data")
}

/// Nod views read through the compressed-body projection, which trails the block
/// that wrote the body, so a fresh write can answer an error for a moment.
fn read_nod_view<C>(world: &World, call: &C, what: &str) -> C::Return
where
    C: SolCall,
    C::Return: Send + 'static,
{
    let url = world.rpc.url(world.validators.primary_port());
    let deadline = Instant::now() + READ_TIMEOUT;
    loop {
        match eth::read_call_result(&url, addresses::NOD_ADDR, call) {
            Ok(value) => return value,
            Err(error) => {
                assert!(Instant::now() < deadline, "{what}: {error}");
                sleep(Duration::from_secs(1));
            }
        }
    }
}

fn wait_for_nod_of(url: &str, owner: Address) -> U256 {
    let mut id = None;
    poll_until(
        ISSUANCE_TIMEOUT,
        || format!("the Nod issued to {owner} never reached its owner's index"),
        || {
            id = eth::read_call(
                url,
                addresses::NOD_ADDR,
                &eth::INod::tokenOfOwnerByIndexCall {
                    owner,
                    index: U256::ZERO,
                },
            );
            id.is_some()
        },
    );
    id.expect("owner's Nod id")
}
