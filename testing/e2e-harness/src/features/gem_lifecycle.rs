//! What a gem supplies to the shared lifecycle: a merchant parks an Intex into a position,
//! issues one gem to each of five owners, and the position returns what it never issued.

use outbe_primitives::time::SECONDS_PER_DAY;
use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use cucumber::{given, then, when};
use outbe_gem::{GemGroupState, GemItemState};

use crate::features::entity_lifecycle::chain::{
    assert_single_event, finalized_checkpoint, head_time, poll_until, verify_checkpoint,
};
use crate::features::entity_lifecycle::entity::{Item, Lifecycle, Phase, Target, Terms};
use crate::features::entity_lifecycle::holders::{self, FORFEITED, HOLDERS, UNPAID_AT_CALL};
use crate::features::entity_lifecycle::markets::MYR_ISO;
use crate::features::entity_lifecycle::phases::CALL_WINDOW_SEED_DAYS;
use crate::features::entity_lifecycle::redeem::{self, mint_authorization, Ledger, Mined};
use crate::features::settlement::{assert_mined_success, find_mining_pow_nonce};
use crate::internal::{addresses, eth};
use crate::world::forge::DEPLOYER_KEY;
use crate::world::settlement_currency::USD_ISO;
use crate::world::test_issuance::{self, SeriesSpec};
use crate::world::{venue_probes, World};

/// The source series' entry price; the gems derive their own from it. Low enough that
/// the controlled COEN/USD quote clears the call price.
const ENTRY_PRICE_MINOR: u64 = 800_000;
/// PROMIS-units per Intex unit, on the wire scale.
const PROMIS_LOAD_MINOR: u128 = 100_000;
/// Units minted to the merchant, and how many of them are parked. Parking part
/// of the holding keeps the burn visible against what stays.
const UNITS: u32 = 8;
const PARKED_UNITS: u32 = 6;
/// Load per issued gem: five leave capacity unissued, and a load short of a whole unit
/// leaves its cost a remainder to floor.
const GEM_LOAD_MINOR: u128 = 100_003;
/// USD (840) as the reference, spelled `U` in the series id.
const REFERENCE_BYTE: u8 = b'U';
/// `getGem` states: Issued, Called, Settled, Forfeited. Forfeited is derived on read.
const ISSUED: u8 = 0;
const CALLED: u8 = 1;
const SETTLED: u8 = 2;
const FORFEITED_STATE: u8 = 3;
const HOLDER_SEED: u64 = 0x0e6e_0000;
/// Issuance mints through a message, not inside the issuing call.
const ISSUANCE_TIMEOUT_SECS: u64 = 180;
/// The call sweep is on a shortened cadence, not instant.
const CALL_TIMEOUT_SECS: u64 = 300;
/// Once the group is closed the sweep reaches the gem in the next block or two.
const FORFEIT_TIMEOUT: Duration = Duration::from_secs(120);
/// The queue determines eligibility from the UTC day that contains the exact deadline.
const EXPIRY_DAY_SECS: u64 = 86_400;
/// A longer notice means the DEV parameter profile is not active.
const MAX_DEV_NOTICE_SECS: u64 = 3_600;
/// Slack past the deadline so the notice is spent before the group is closed.
const EXPIRY_MARGIN_SECS: u64 = 30;
/// After the test prepares the pass for the position's deadline day, the sweep reaches the position in later blocks.
const POSITION_SWEEP_TIMEOUT_SECS: u64 = 120;
/// The DEV validity (15 minutes) runs from parking, and most of it is spent
/// waiting out the call notice before this step is reached.
const POSITION_DEADLINE_TIMEOUT_SECS: u64 = 900;

pub(crate) struct GemLifecycle;

/// Ten real Oracle publications and restart slack must fit inside one pricing hour.
const HOURLY_PRICING_ORACLE_PERIOD: u64 = 8;

#[given(expr = "a fresh Gem lifecycle localnet with a {int}-block voting window")]
fn fresh_gem_lifecycle_localnet(world: &mut World, window: u64) {
    crate::features::common::boot_localnet(
        world,
        window,
        &[(
            "TESTNET_ORACLE_VOTE_PERIOD_BLOCKS",
            HOURLY_PRICING_ORACLE_PERIOD.to_string(),
        )],
    );
    let checkpoint = finalized_checkpoint(world);
    for port in world.validators.committee_ports() {
        let params = eth::read_call_at_result(
            &world.rpc.url(port),
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &eth::IOracle::getParamsCall {},
            checkpoint.height,
        )
        .expect("read the executed Gem fixture Oracle parameters");
        assert_eq!(
            params.votePeriod, HOURLY_PRICING_ORACLE_PERIOD,
            "Gem fixture Oracle period on port {port}"
        );
    }
    verify_checkpoint(world, checkpoint);
}

#[when("a test Intex series is issued to the merchant in MYR against USD")]
fn issue_source_series(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    let merchant = crate::world::origin_venue::deployer_address();
    let issuance_currency = world
        .state
        .issuance_market
        .expect("the gem scenario prices an issuance market");
    assert_eq!(
        issuance_currency, MYR_ISO,
        "the source series is issued in MYR"
    );

    let origin_router = world
        .state
        .origin_contracts
        .as_ref()
        .expect("the Intex engine was deployed")
        .origin_router;
    let head = head_time(world);
    let day = outbe_primitives::time::worldwide_day_from_timestamp(head);
    let now = u32::try_from(head).expect("timestamp fits a uint32");

    test_issuance::open_day(test_issuance::OpenDay {
        url: &url,
        sender_key: DEPLOYER_KEY,
        origin_router,
        worldwide_day: day,
        now,
        reference_currency: USD_ISO,
        entry_price_minor: ENTRY_PRICE_MINOR,
        promis_load_minor: PROMIS_LOAD_MINOR,
    })
    .expect("open the day the source series is issued into");

    // One chain, one series: gems never leave the committee, so this scenario
    // needs neither a second venue nor a relay.
    let series = test_issuance::issue_series(test_issuance::IssueSeries {
        url: &url,
        sender_key: DEPLOYER_KEY,
        worldwide_day: day,
        // A closed day, so the merchant gems can read its price.
        emission_day: outbe_primitives::time::previous_date_key(
            outbe_primitives::time::timestamp_to_date_key(head),
        )
        .expect("date step"),
        issued_at: head,
        reference_currency: USD_ISO,
        reference_byte: REFERENCE_BYTE,
        entry_price_minor: U256::from(ENTRY_PRICE_MINOR),
        promis_load_minor: PROMIS_LOAD_MINOR,
        issued_units: UNITS,
        chains: &[u32::try_from(chain_id).expect("committee chain id fits a uint32")],
        specs: &[SeriesSpec {
            issuance: *b"MYR",
            issuance_currency,
        }],
    })
    .expect("issue the source series");

    let series = *series.first().expect("one series was issued");

    // Issuance reaches the collection as its own message; parking before the
    // units land reverts with NonexistentToken.
    let nft = intex_nft(world);
    test_issuance::issue_to_owner(test_issuance::IssueToOwner {
        url: &url,
        nft,
        admin_key: DEPLOYER_KEY,
        owner: merchant,
        units: UNITS,
        series: &[series],
        tick: || {},
        timeout: Duration::from_secs(ISSUANCE_TIMEOUT_SECS),
    })
    .expect("issue the source units to the merchant");
    let deadline = Instant::now() + Duration::from_secs(ISSUANCE_TIMEOUT_SECS);
    loop {
        if venue_probes::series_balances(&url, nft, series, merchant) == Some((u64::from(UNITS), 0))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "series {series} never minted its units to the merchant"
        );
        sleep(Duration::from_secs(2));
    }

    world.state.gem_source_series = Some(series);
}

#[when("the merchant parks part of their units into a gem position")]
fn park_units(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let merchant = crate::world::origin_venue::deployer_address();
    let series = source_series(world);

    let call = eth::IGemFactory::issueGemPositionCall {
        sourceIntexId: series,
        units: U256::from(PARKED_UNITS),
    };
    // The precompile reports a decode failure rather than the collection's own
    // revert, so simulate first: that keeps the real reason in the failure.
    if let Err(error) = eth::simulate_call(&url, addresses::GEM_FACTORY_ADDR, merchant, &call) {
        panic!("parking units reverts: {error}");
    }
    let parked =
        eth::send_call_outcome(&url, addresses::GEM_FACTORY_ADDR, DEPLOYER_KEY, &call, None)
            .expect("park units into a gem position");
    assert_mined_success(&parked, "park units into a gem position");

    // The position is a single-owner NFT, and this is the merchant's first.
    let position_id = eth::read_call(
        &url,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::tokenOfOwnerByIndexCall {
            owner: merchant,
            index: U256::ZERO,
        },
    )
    .expect("the parked position was issued to the merchant");
    world.state.gem_position = Some(position_id);
}

#[then("the position holds the parked capacity and the units are burned")]
fn position_holds_capacity(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let merchant = crate::world::origin_venue::deployer_address();
    let nft = intex_nft(world);
    let series = source_series(world);
    let position = read_position(world);

    assert_eq!(
        position.remainingCapacityMinor,
        U256::from(PROMIS_LOAD_MINOR) * U256::from(PARKED_UNITS),
        "the position did not take the parked units' whole load as capacity"
    );
    assert_eq!(
        position.merchant, merchant,
        "the position was issued to somebody other than the merchant who parked"
    );
    assert!(
        position.expiresAt > position.issuedAt,
        "the position was parked without a deadline to expire at"
    );
    assert_eq!(
        venue_probes::series_balances(&url, nft, series, merchant),
        Some((u64::from(UNITS - PARKED_UNITS), 0)),
        "parking did not burn exactly the units it took"
    );
}

#[when("the merchant issues a gem to each of five owners, leaving capacity unissued")]
fn issue_five_gems(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let position_id = world.state.gem_position.expect("a position was parked");
    holders::fund(world, HOLDER_SEED);

    test_issuance::seed_day_vwaps(
        &url,
        DEPLOYER_KEY,
        USD_ISO,
        1,
        U256::from(ENTRY_PRICE_MINOR),
    )
    .expect("seed the previous day's VWAP");
    let mut gems = Vec::with_capacity(HOLDERS);
    for index in 0..HOLDERS {
        let issued = eth::send_call_outcome(
            &url,
            addresses::GEM_FACTORY_ADDR,
            DEPLOYER_KEY,
            &eth::IGemFactory::issueGemCall {
                positionId: position_id,
                owner: owner_address(index),
                promisLoadMinor: U256::from(GEM_LOAD_MINOR),
                issuanceCurrency: MYR_ISO,
                referenceCurrency: USD_ISO,
            },
            None,
        )
        .expect("issue a merchant gem");
        assert_mined_success(&issued, "issue a merchant gem");
        let gem_id = eth::receipt_event::<eth::IGemFactory::GemIssued>(
            &issued.receipt,
            addresses::GEM_FACTORY_ADDR,
        )
        .gemId;
        // The call counts only days a gem held in full: stamp it behind every day
        // the scenario seeds.
        let backdated = eth::send_call_outcome(
            &url,
            addresses::GEM_FACTORY_ADDR,
            DEPLOYER_KEY,
            &IGemFactoryTestArming::backdateGemForTestCall {
                gemId: gem_id,
                issuedAt: head_time(world)
                    .saturating_sub((u64::from(CALL_WINDOW_SEED_DAYS) + 1) * SECONDS_PER_DAY),
            },
            None,
        )
        .expect("backdate the gem's issuance stamp");
        assert_mined_success(&backdated, "backdate the gem's issuance stamp");
        gems.push(gem_id);
    }
    world.state.lifecycle_gems = gems;

    // The forfeit and the position's expiry may credit the pool in either order:
    // measure both from before either, while the position still holds its rest.
    let height = finalized_checkpoint(world).height;
    let position = eth::read_call_at(
        &url,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::getPositionCall {
            positionId: position_id,
        },
        height,
    )
    .expect("position at the pool baseline");
    assert_eq!(position.remainingCapacityMinor, unissued_capacity());
    let pool = eth::read_call_at(
        &url,
        addresses::PROMIS_LIMIT_ADDR,
        &eth::IPromisLimit::promisLimitCall {},
        height,
    )
    .expect("unallocated pool at the baseline");
    world.state.entity_lifecycle.pool_before_forfeit = Some((height, pool));
}

#[then("every validator reads the same gem and group from one finalized EVM state")]
fn gem_bodies_match_on_every_committee_port(world: &mut World) {
    let minimum = issuance_baseline_height(world);
    for index in 0..HOLDERS {
        let id = gem(world, index);
        let view = read_gem(world, id);
        let (item, group) = gem_bodies(world, id, minimum);
        assert_eq!(item.gem_id, id);
        assert_eq!(item.owner, owner_address(index));
        assert_eq!(item.promis_load_minor, view.promisLoadMinor);
        assert_eq!(item.issued_at, view.issuedAt);
        assert_eq!(group.group_key, item.group_key);
        assert_eq!(item.entry_price_minor, view.entryPriceMinor);
        assert_eq!(group.reference_currency, item.reference_currency);
    }
}

impl Lifecycle for GemLifecycle {
    fn call_price(&self, world: &World) -> U256 {
        read_gem(world, gem(world, 0)).callPriceMinor
    }

    fn terms(&self, world: &World, item: &Item) -> Terms {
        let Item::Gem(id) = item else {
            unreachable!("a gem scenario pays only for gems")
        };
        let data = read_gem(world, *id);
        Terms {
            entry_price: data.entryPriceMinor,
            load: data.promisLoadMinor,
        }
    }

    fn assert_issued(&self, world: &World) {
        let position = read_position(world);
        assert_eq!(
            position.remainingCapacityMinor,
            unissued_capacity(),
            "issuing the gems did not drain exactly their load from the position"
        );
        // Never below the source Intex, valued on its emission day.
        let source_floor = eth::read_call(
            &world_url(world),
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &eth::IOracle::getUtcDayVwapCall {
                base: Address::ZERO,
                quote: outbe_primitives::asset_type::currency_address(USD_ISO),
                utcDay: outbe_primitives::series_id::SeriesId::from_bytes(position.sourceIntexId.0)
                    .day(),
            },
        )
        .expect("the source emission day is priced");
        let first = read_gem(world, gem(world, 0));
        for index in 0..HOLDERS {
            let id = gem(world, index);
            let data = read_gem(world, id);
            assert_eq!(data.state, ISSUED, "gem {id} was not born Issued");
            assert_eq!(data.calledAt, 0);
            assert_eq!(
                data.owner,
                owner_address(index),
                "gem {id} went to the wrong owner"
            );
            assert_eq!(data.promisLoadMinor, U256::from(GEM_LOAD_MINOR));
            assert_eq!(
                (data.issuanceCurrency, data.referenceCurrency),
                (MYR_ISO, USD_ISO),
                "gem {id} does not carry the currencies the merchant named"
            );
            assert!(
                data.entryPriceMinor >= source_floor,
                "gem {id} priced below the Intex it was parked from"
            );
            assert_eq!(
                (data.entryPriceMinor, data.callPriceMinor),
                (first.entryPriceMinor, first.callPriceMinor),
                "every gem must share one set of terms"
            );
        }
    }

    fn targets(&self, world: &World, phase: Phase) -> [Target; 2] {
        holders::paid_in(phase).map(|index| target(world, index))
    }

    fn called(&self, world: &World) -> bool {
        let unpaid = UNPAID_AT_CALL.map(|index| read_gem(world, gem(world, index)));
        unpaid.iter().all(|data| data.state == CALLED)
            && unpaid
                .iter()
                .all(|data| head_time(world) <= data.calledAt + u64::from(data.callNoticePeriod))
    }

    fn assert_paid_settled(&self, world: &World) {
        for paid in &world.state.entity_lifecycle.payments {
            let Item::Gem(id) = paid.target.item else {
                unreachable!("a gem scenario pays only for gems")
            };
            assert_eq!(
                read_gem(world, id).state,
                SETTLED,
                "paid gem {id} left Settled"
            );
        }
    }

    /// Wait until the exact notice expires.
    /// Prepare the pass for a closed deadline day in the test build.
    fn lapse_notice(&self, world: &mut World) {
        let url = world_url(world);
        let id = gem(world, FORFEITED);
        let called = read_gem(world, id);
        assert!(
            matches!(called.state, CALLED | FORFEITED_STATE),
            "gem {id} reads state {} after its call",
            called.state
        );
        let notice = u64::from(called.callNoticePeriod);
        assert!(
            notice <= MAX_DEV_NOTICE_SECS,
            "call notice is {notice}s: the DEV parameter profile is not active, so this \
             scenario would wait out the production window"
        );
        let notice_end = called.calledAt + notice + EXPIRY_MARGIN_SECS;
        poll_until(
            Duration::from_secs(notice + CALL_TIMEOUT_SECS),
            || format!("gem {id} never reached its call deadline {notice_end}"),
            || head_time(world) >= notice_end,
        );
        match eth::read_call(
            &url,
            addresses::GEM_ADDR,
            &eth::IGem::getGemCall { gemId: id },
        ) {
            Some(lapsed) => assert_eq!(
                lapsed.state, FORFEITED_STATE,
                "gem {id} past its call deadline must read Forfeited until the sweep burns it"
            ),
            None => assert!(
                gem_count(&url, owner_address(FORFEITED)).is_zero(),
                "gem {id} is unreadable past its call deadline but was not burned"
            ),
        }
        let closed = eth::send_call_outcome(
            &url,
            addresses::GEM_FACTORY_ADDR,
            DEPLOYER_KEY,
            &IGemFactoryTestArming::closeCallNoticeForTestCall {
                gemId: id,
                deadline: head_time(world).saturating_sub(EXPIRY_DAY_SECS),
            },
            None,
        )
        .expect("arm the gem's closed deadline day");
        assert_mined_success(&closed, "arm the gem's closed deadline day");
    }

    fn assert_forfeited(&self, world: &World) {
        let url = world_url(world);
        let id = gem(world, FORFEITED);
        poll_until(
            FORFEIT_TIMEOUT,
            || format!("gem {id} outlived its call notice; the forfeit sweep never burned it"),
            || gem_count(&url, owner_address(FORFEITED)).is_zero(),
        );
        assert!(
            eth::read_call(
                &url,
                addresses::GEM_ADDR,
                &eth::IGem::getGemCall { gemId: id }
            )
            .is_none(),
            "a forfeited gem must not be readable"
        );
        let height = finalized_checkpoint(world).height;
        for port in world.validators.committee_ports() {
            assert!(
                world
                    .rpc
                    .gem_state_at(
                        port,
                        world
                            .rpc
                            .checkpoint_at(world.validators.primary_port(), height)
                            .expect("forfeit EVM checkpoint"),
                        id
                    )
                    .expect("forfeited Gem state")
                    .is_none(),
                "validator {port} still serves the forfeited gem body"
            );
        }
        assert_expiry_returns(world, height, false);
    }

    fn mine_paid(&self, world: &World) -> Vec<Mined> {
        let url = world_url(world);
        let load = U256::from(GEM_LOAD_MINOR);
        holders::paid()
            .map(|index| {
                let key = owner_key(index);
                let owner = owner_address(index);
                let id = gem(world, index);
                let before = redeem::balance(world, Ledger::Promis, &key, owner);
                let (mac, op_nonce) = mint_authorization(world, Ledger::Promis, &key, owner, load);
                let outcome = eth::send_call_outcome(
                    &url,
                    addresses::GEM_FACTORY_ADDR,
                    &key,
                    &eth::IGemFactory::minePromisCall {
                        gemId: id,
                        nonce: find_mining_pow_nonce(
                            outbe_common::pow::MiningDomain::Gem,
                            id,
                            owner,
                        ),
                        mac,
                        opNonce: op_nonce,
                    },
                    None,
                )
                .expect("submit Promis mining");
                assert_mined_success(&outcome, "mine Promis from the paid gem");
                assert!(
                    gem_count(&url, owner).is_zero(),
                    "mining did not burn gem {id}"
                );
                Mined {
                    owner,
                    owner_key: key,
                    ledger: Ledger::Promis,
                    before,
                    amount: load,
                }
            })
            .collect()
    }
}

#[when("the position's validity runs out")]
fn wait_for_position_expiry(world: &mut World) {
    let expires_at = read_position(world).expiresAt;
    poll_until(
        Duration::from_secs(POSITION_DEADLINE_TIMEOUT_SECS),
        || format!("the chain never reached the position's deadline {expires_at}"),
        || head_time(world) > expires_at,
    );
}

#[then("the position returns its unissued capacity to the same pool")]
fn position_returns_capacity(world: &mut World) {
    let url = world_url(world);
    let position_id = world.state.gem_position.expect("a position was parked");

    // Hour groups retain their identity but return capacity after their deadline day closes.
    // Prepare the test pass for a closed day unless cleanup already deleted the position.
    let read_position = || {
        eth::read_call(
            &url,
            addresses::GEM_FACTORY_ADDR,
            &eth::IGemFactory::getPositionCall {
                positionId: position_id,
            },
        )
    };
    if read_position().is_some() {
        let closed = eth::send_call_outcome(
            &url,
            addresses::GEM_FACTORY_ADDR,
            DEPLOYER_KEY,
            &IGemFactoryTestArming::closePositionHourForTestCall {
                positionId: position_id,
                deadline: head_time(world).saturating_sub(EXPIRY_DAY_SECS),
            },
            None,
        )
        .expect("arm the position group's closed deadline day");
        if read_position().is_some() {
            assert_mined_success(&closed, "arm the position group's closed deadline day");
        }
    }
    poll_until(
        Duration::from_secs(POSITION_SWEEP_TIMEOUT_SECS),
        || format!("position {position_id} is past its deadline; the cleanup has not deleted it"),
        || read_position().is_none(),
    );

    assert_expiry_returns(world, finalized_checkpoint(world).height, true);
}

fn unissued_capacity() -> U256 {
    U256::from(PROMIS_LOAD_MINOR) * U256::from(PARKED_UNITS)
        - U256::from(GEM_LOAD_MINOR) * U256::from(HOLDERS)
}

/// Since the baseline the pool gained exactly the forfeited gem's load, plus the
/// position's unissued rest once the position has expired too.
fn assert_expiry_returns(world: &World, height: u64, require_position_expiry: bool) {
    let url = world_url(world);
    let (from, before) = world
        .state
        .entity_lifecycle
        .pool_before_forfeit
        .expect("the pool was read before either return");
    let position_id = world.state.gem_position.expect("parked position");
    let expiry_hour = eth::read_call_at(
        &url,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::getPositionCall {
            positionId: position_id,
        },
        from,
    )
    .expect("the position before either return")
    .expiresAt
        / 3_600;
    let position_expired = hour_expired(&url, expiry_hour, from, height);
    if require_position_expiry {
        assert!(position_expired, "position retained unissued capacity");
    }
    let group = eth::read_call_at(
        &url,
        addresses::GEM_ADDR,
        &eth::IGem::getGemCall {
            gemId: gem(world, FORFEITED),
        },
        from,
    )
    .expect("the forfeited gem was live before its group was forfeited")
    .groupKey;
    assert_single_event(
        &url,
        addresses::GEM_ADDR,
        from,
        height,
        eth::IGem::GemGroupForfeited {
            groupKey: group,
            gems: 1,
            promisLoadMinor: U256::from(GEM_LOAD_MINOR),
        },
    );
    let returned = if position_expired {
        assert_single_event(
            &url,
            addresses::GEM_FACTORY_ADDR,
            from,
            height,
            eth::IGemFactory::GemPositionGroupExpired {
                expiryHour: expiry_hour,
                positions: 1,
                returnedCapacityMinor: unissued_capacity(),
            },
        );
        U256::from(GEM_LOAD_MINOR) + unissued_capacity()
    } else {
        U256::from(GEM_LOAD_MINOR)
    };
    assert_eq!(
        eth::read_call_at(
            &url,
            addresses::PROMIS_LIMIT_ADDR,
            &eth::IPromisLimit::promisLimitCall {},
            height
        ),
        Some(before + returned),
        "expiry returns did not credit the exact unallocated load"
    );
}

/// Whether the positions of `hour` returned their capacity within the blocks.
fn hour_expired(url: &str, hour: u64, from: u64, to: u64) -> bool {
    use alloy_sol_types::SolEvent;
    let logs = eth::raw_json_result(
        url,
        "eth_getLogs",
        serde_json::json!([{
            "address": addresses::GEM_FACTORY_ADDR,
            "fromBlock": format!("0x{from:x}"), "toBlock": format!("0x{to:x}"),
            "topics": [
                eth::IGemFactory::GemPositionGroupExpired::SIGNATURE_HASH,
                alloy_primitives::B256::from(alloy_primitives::U256::from(hour)),
            ],
        }]),
    )
    .expect("position expiry logs");
    logs.as_array().is_some_and(|logs| !logs.is_empty())
}

alloy_sol_types::sol! {
    interface IGemFactoryTestArming {
        function backdateGemForTest(uint256 gemId, uint64 issuedAt) external;
        function closeCallNoticeForTest(uint256 gemId, uint64 deadline) external;
        function closePositionHourForTest(uint256 positionId, uint64 deadline) external;
    }
}

/// The finalized height recorded right after the five issuances; every body proof must
/// select a header at or past it.
fn issuance_baseline_height(world: &World) -> u64 {
    world
        .state
        .entity_lifecycle
        .pool_before_forfeit
        .expect("the gems were issued")
        .0
}

/// Reads the exact Gem and group state on every validator at one checkpoint.
fn gem_bodies(world: &World, id: U256, minimum: u64) -> (GemItemState, GemGroupState) {
    let checkpoint = finalized_checkpoint(world);
    assert!(checkpoint.height >= minimum);
    let mut expected = None;
    for port in world.validators.committee_ports() {
        let (item, group) = world
            .rpc
            .gem_state_at(port, checkpoint, id)
            .expect("Gem EVM state")
            .expect("Gem is present");
        let bodies = (item, group);
        assert_eq!(
            *expected.get_or_insert(bodies.clone()),
            bodies,
            "all-validator Gem state parity"
        );
    }
    expected.expect("nonempty validator cohort")
}

fn world_url(world: &World) -> String {
    world.rpc.url(world.validators.primary_port())
}

fn owner_key(index: usize) -> String {
    holders::key(HOLDER_SEED, index)
}

fn owner_address(index: usize) -> Address {
    holders::address(HOLDER_SEED, index)
}

fn gem(world: &World, index: usize) -> U256 {
    world.state.lifecycle_gems[index]
}

fn target(world: &World, index: usize) -> Target {
    Target {
        item: Item::Gem(gem(world, index)),
        owner: owner_address(index),
        owner_key: owner_key(index),
        issuance_currency: MYR_ISO,
    }
}

fn source_series(world: &World) -> alloy_primitives::FixedBytes<14> {
    world
        .state
        .gem_source_series
        .expect("the source series was issued")
}

fn intex_nft(world: &World) -> Address {
    world
        .state
        .origin_contracts
        .as_ref()
        .expect("the Intex engine was deployed")
        .intex_nft
}

fn read_position(world: &World) -> eth::IGemFactory::PositionData {
    eth::read_call(
        &world_url(world),
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::getPositionCall {
            positionId: world.state.gem_position.expect("a position was parked"),
        },
    )
    .expect("the parked position reads back")
}

fn read_gem_view<C>(url: &str, call: &C, what: &str) -> C::Return
where
    C: alloy_sol_types::SolCall,
    C::Return: Send + 'static,
{
    eth::read_call_result(url, addresses::GEM_ADDR, call)
        .unwrap_or_else(|error| panic!("{what}: {error}"))
}

fn gem_count(url: &str, owner: Address) -> U256 {
    read_gem_view(
        url,
        &eth::IGem::balanceOfCall { owner },
        "the gem collection answers a balance read",
    )
}

fn read_gem(world: &World, gem_id: U256) -> eth::IGem::GemData {
    read_gem_view(
        &world_url(world),
        &eth::IGem::getGemCall { gemId: gem_id },
        &format!("gem {gem_id} does not read back"),
    )
}
