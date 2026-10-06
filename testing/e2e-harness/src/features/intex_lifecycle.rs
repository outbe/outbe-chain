//! What an Intex supplies to the shared lifecycle: four series of one day on two chains;
//! two are paid in two parts each, one runs out after a partial payment, one untouched.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, FixedBytes, U256};
use alloy_sol_types::sol;
use base64::Engine as _;

use crate::internal::{addresses, eth};
use cucumber::{then, when};

use crate::features::entity_lifecycle::chain::{
    assert_single_event, finalized_checkpoint, head_time, poll_until,
};
use crate::features::entity_lifecycle::entity::{Item, Lifecycle, Phase, Rail, Target, Terms};
use crate::features::entity_lifecycle::markets::{EUR_ISO, MYR_ISO};
use crate::features::entity_lifecycle::payment;
use crate::features::entity_lifecycle::redeem::{self, mint_authorization, Ledger, Mined};
use crate::world::forge::DEPLOYER_KEY;
use crate::world::relay::{Relay, RelayEnd};
use crate::world::settlement_currency::USD_ISO;
use crate::world::test_issuance::{self, SeriesSpec};
use crate::world::{venue_probes, World};

sol! {
    interface IIntexCard {
        function issuedTokenId(bytes14 seriesId) external pure returns (uint256);
        function uri(uint256 tokenId) external view returns (string memory);
        function vwapSource() external view returns (address);
    }

    interface IVwapSource {
        function maxUtcDayVwapSince(uint16 isoCode, uint32 fromUtcDay) external view returns (uint256);
    }
}

/// Shared by every series so one sweep pass calls them together, and low enough that the
/// committee's close clears the call price.
const ENTRY_PRICE_MINOR: u64 = 800_000;
/// PROMIS-units per Intex unit, on the wire scale; not a whole unit, so a cost leaves a
/// remainder to floor.
const PROMIS_LOAD_MINOR: u128 = 100_003;
/// Issuance currencies of the two series left to run out.
const GBP_ISO: u16 = 826;
const JPY_ISO: u16 = 392;
/// Units each series mints per chain. The holding is split so bringing units home
/// is a real step rather than a formality.
const COMMITTEE_UNITS: u32 = 4;
const TARGET_UNITS: u32 = 6;
/// Brought home while the series are still tradable; the rest travels under Called,
/// where the bridge admits a move only to the owner's own address.
const TRADABLE_HOP_UNITS: u32 = 2;
const UNITS: u32 = COMMITTEE_UNITS + TARGET_UNITS;
/// What is home to pay for while qualified, and what the second hop brings.
const QUALIFIED_UNITS: u32 = COMMITTEE_UNITS + TRADABLE_HOP_UNITS;
const CALLED_UNITS: u32 = TARGET_UNITS - TRADABLE_HOP_UNITS;
/// USD (840) as the reference for every series, spelled `U` in the series id.
const REFERENCE_BYTE: u8 = b'U';
/// Long enough for the chain to close a one-day gap, which it does per block.
const CATCH_UP_TIMEOUT_SECS: u64 = 900;
/// The sender fires every minute in e2e, then the relay carries the day over.
const VWAP_PUSH_TIMEOUT_SECS: u64 = 600;
/// `IntexState::Issued` / `Qualified` / `Called`.
const ISSUED: u8 = 0;
const QUALIFIED: u8 = 1;
const CALLED: u8 = 2;
/// Derived against the clock on both chains; never written by anything.
const EXPIRED: u8 = 3;
/// Slack past the deadline so the sweep has a block to run in.
const EXPIRY_MARGIN_SECS: u64 = 30;
/// The expiry queue buckets deadlines by the hour they fall in.
const EXPIRY_BUCKET_SECS: u64 = 3_600;
/// Settled out of the expiring series, so the forfeit is the tirage less these.
const EXPIRING_SETTLED_UNITS: u32 = 2;
/// The credit lands in the block the sweep reaches the queue head.
const FORFEIT_TIMEOUT: Duration = Duration::from_secs(180);
/// How far back the series are issued so closed days exist after their issuance.
const CALL_LOOKBACK_DAYS: u32 = 3;
/// A relayed message is asynchronous; scenarios wait for arrival rather than assume it.
const DELIVERY_TIMEOUT_SECS: u64 = 180;

pub(crate) struct IntexLifecycle;

#[when("four test Intex series sharing a reference currency are issued to the owner")]
fn issue_four_series(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    let owner = owner();
    assert_eq!(
        world.state.issuance_market,
        Some(MYR_ISO),
        "the paid series is issued in MYR"
    );

    let origin_router = world
        .state
        .origin_contracts
        .as_ref()
        .expect("the Intex engine was deployed")
        .origin_router;

    // The router addresses an issuance leg only to a chain the day was started on,
    // so the day has to be opened before anything can be issued into it.
    // Issued into a day already behind us: the call sweep counts breach days only
    // from the issuance day forward, and only closed days exist to count.
    let day = chain_worldwide_day_offset(world, port, -(i64::from(CALL_LOOKBACK_DAYS) * 86_400));
    let now = u32::try_from(head_time(world)).expect("timestamp fits a uint32");
    test_issuance::open_day(
        &url,
        DEPLOYER_KEY,
        origin_router,
        day,
        now,
        USD_ISO,
        ENTRY_PRICE_MINOR,
        PROMIS_LOAD_MINOR,
    )
    .expect("open the day the series are issued into");

    // Same day and reference currency, different issuance currencies: one group,
    // four members, which is what makes the group promotion and the mark batch real.
    let series = test_issuance::issue_series(
        &url,
        DEPLOYER_KEY,
        day,
        // Issuance is stamped where the seeded days already lie behind it.
        u32::try_from(head_time(world).saturating_sub(u64::from(CALL_LOOKBACK_DAYS) * 86_400))
            .expect("backdated stamp fits a uint32"),
        USD_ISO,
        REFERENCE_BYTE,
        U256::from(ENTRY_PRICE_MINOR),
        PROMIS_LOAD_MINOR,
        owner,
        &[COMMITTEE_UNITS, TARGET_UNITS],
        &[
            u32::try_from(chain_id).expect("committee chain id fits a uint32"),
            u32::try_from(world.target_chain.chain_id()).expect("target chain id fits a uint32"),
        ],
        &[
            SeriesSpec {
                issuance: *b"MYR",
                issuance_currency: MYR_ISO,
            },
            SeriesSpec {
                issuance: *b"EUR",
                issuance_currency: EUR_ISO,
            },
            // Only part of this one is settled, so it is still holding units when the
            // notice runs out.
            SeriesSpec {
                issuance: *b"GBP",
                issuance_currency: GBP_ISO,
            },
            // Nobody touches this one at all, so the sweep forfeits its whole tirage
            // and the two together prove the subtraction rather than one case of it.
            SeriesSpec {
                issuance: *b"JPY",
                issuance_currency: JPY_ISO,
            },
        ],
    )
    .expect("issue the lifecycle series");

    // The capacity committee runs ahead of wall time, so advance Anvil only
    // after all legs have their timestamps fixed.
    let issued_through = head_time(world);
    let target_url = target_rpc_url(world);
    if eth::latest_block_timestamp(&target_url).expect("target timestamp before delivery")
        < issued_through
    {
        world
            .target_chain
            .sync_clock_to(issued_through)
            .expect("synchronize target time after issuance");
    }
    assert!(
        eth::latest_block_timestamp(&target_url)
            .expect("verify target timestamp after synchronization")
            >= issued_through,
        "target clock remains behind the issuing committee block"
    );

    let mut series = series;
    let untouched = series.pop().expect("the untouched series was issued last");
    let expiring = series
        .pop()
        .expect("the expiring series was issued next to last");
    world.state.lifecycle_series = series;
    world.state.expiring_series = Some(expiring);
    world.state.untouched_series = Some(untouched);
    world.state.lifecycle_day = Some(day);
}

#[then("the owner holds issued units of every series on each chain")]
fn owner_holds_issued_units(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let target_url = target_rpc_url(world);
    let nft = intex_nft(world);
    let target_nft = target_intex_nft(world);
    let owner = owner();

    assert_eq!(
        world.state.lifecycle_series.len(),
        2,
        "the scenario pays for two series"
    );

    // The target-chain leg travels as a real message, so give the relay its round.
    let deadline = Instant::now() + Duration::from_secs(DELIVERY_TIMEOUT_SECS);
    for series in world.state.lifecycle_series.clone() {
        assert!(
            venue_probes::series_exists(&url, nft, series),
            "the committee collection does not know series {series}"
        );
        assert_eq!(
            venue_probes::series_balances(&url, nft, series, owner),
            Some((u64::from(COMMITTEE_UNITS), 0)),
            "series {series} did not mint its committee units to the owner"
        );
        loop {
            if venue_probes::series_balances(&target_url, target_nft, series, owner)
                == Some((u64::from(TARGET_UNITS), 0))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "series {series} did not reach its exact target-chain balance before the delivery deadline"
            );
            // The committee runs on a logical clock days ahead of real time, and the
            // issuance carries its stamps: the target rejects them as its own future
            // until its clock is carried over, and the relay retries in silence.
            carry_target_clock(world);
            sleep(Duration::from_secs(2));
        }
    }
}

/// Carry the committee's logical clock over to the target chain. Nothing on a
/// localnet plays the operator who keeps a second chain in step, and a stamp that
/// sits in the target's future is refused rather than queued.
fn carry_target_clock(world: &World) {
    let Some(now) = world
        .rpc
        .latest_block_timestamp(world.validators.primary_port())
    else {
        return;
    };
    let _ = world.target_chain.sync_clock_to(now);
}

/// The origin's collection reads the IntexFactory; the target's reads the registry the origin pushes to.
#[then("every series card reads Qualified on both chains")]
fn cards_read_qualified(world: &mut World) {
    let chains = [
        (
            world.rpc.url(world.validators.primary_port()),
            intex_nft(world),
        ),
        (target_rpc_url(world), target_intex_nft(world)),
    ];
    let deadline = Instant::now() + Duration::from_secs(VWAP_PUSH_TIMEOUT_SECS);

    for (url, nft) in &chains {
        let source = eth::read_call(url, *nft, &IIntexCard::vwapSourceCall {})
            .expect("the collection names its VWAP source");
        for series in world.state.lifecycle_series.clone() {
            loop {
                let state = card_state(url, *nft, series);
                if state.as_deref() == Some("Qualified") {
                    break;
                }
                let (iso_code, floor, from) = venue_probes::series_floor_terms(url, *nft, series)
                    .expect("the chain knows the series");
                let max = eth::read_call(
                    url,
                    source,
                    &IVwapSource::maxUtcDayVwapSinceCall {
                        isoCode: iso_code,
                        fromUtcDay: from,
                    },
                );
                assert!(
                    Instant::now() < deadline,
                    "the card of {series} on {url} reads {state:?}; its source {source} has {max:?} against floor {floor}"
                );
                // The pushed day carries the committee's stamp, which the target
                // refuses while it lies in the target's future.
                carry_target_clock(world);
                sleep(Duration::from_secs(5));
            }
        }
    }
}

/// The `Series State` trait of the issued class's card.
fn card_state(url: &str, nft: alloy_primitives::Address, series: FixedBytes<14>) -> Option<String> {
    let token = eth::read_call(
        url,
        nft,
        &IIntexCard::issuedTokenIdCall { seriesId: series },
    )?;
    let uri = eth::read_call(url, nft, &IIntexCard::uriCall { tokenId: token })?;
    let json = base64::engine::general_purpose::STANDARD
        .decode(uri.strip_prefix("data:application/json;base64,")?)
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&json).ok()?;
    json["attributes"]
        .as_array()?
        .iter()
        .find(|attribute| attribute["trait_type"] == "Series State")?["value"]
        .as_str()
        .map(str::to_owned)
}

/// Wait for the chain to reach `target` in its own time; it closes the gap per block.
///
/// Reports where it actually got to, and whether it was still moving: a ratchet that
/// stalled and one that is merely slow need different answers.
fn wait_for_chain_time(world: &World, port: u16, target: u64) {
    let deadline = Instant::now() + Duration::from_secs(CATCH_UP_TIMEOUT_SECS);
    let mut first = None;
    loop {
        let now = world.rpc.latest_block_timestamp(port);
        if first.is_none() {
            first = now;
        }
        if now.is_some_and(|now| now >= target) {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "the committee never reached logical time {target}: started at {first:?}, \
                 reached {now:?} (short by {:?}s) after {CATCH_UP_TIMEOUT_SECS}s",
                now.map(|now| target.saturating_sub(now))
            );
        }
        sleep(Duration::from_secs(2));
    }
}

fn intex_nft(world: &World) -> alloy_primitives::Address {
    world
        .state
        .origin_contracts
        .as_ref()
        .expect("the Intex engine was deployed")
        .intex_nft
}

/// The same collection on the target chain, where the other half of every series lives.
fn target_intex_nft(world: &World) -> alloy_primitives::Address {
    world
        .state
        .target_contracts
        .as_ref()
        .expect("the Intex venue was deployed on the target chain")
        .intex_nft
}

fn target_rpc_url(world: &World) -> String {
    world
        .target_chain
        .rpc_url()
        .expect("target chain is running")
}

impl Lifecycle for IntexLifecycle {
    /// The pricing window closes at midnight on this localnet, so the committee has
    /// closed yesterday on its own feed and sent that close to the target chain.
    fn recorded_close(&self, world: &World) -> Option<U256> {
        use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};
        let yesterday = previous_date_key(timestamp_to_date_key(head_time(world)));
        let close = eth::read_call(
            &world.rpc.url(world.validators.primary_port()),
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &eth::IOracle::getUtcDayVwapCall {
                base: Address::ZERO,
                quote: outbe_primitives::asset_type::currency_address(USD_ISO),
                utcDay: yesterday,
            },
        )
        .filter(|vwap| !vwap.is_zero())
        .unwrap_or_else(|| panic!("the committee has no COEN/USD close for {yesterday}"));
        Some(close)
    }

    fn floor(&self, world: &World) -> U256 {
        U256::from(prices(world, paid_series(world)[0]).1)
    }

    fn call_price(&self, world: &World) -> U256 {
        U256::from(prices(world, paid_series(world)[0]).2)
    }

    fn terms(&self, world: &World, item: &Item) -> Terms {
        let Item::Series { id, units } = item else {
            unreachable!("an Intex scenario pays only for series")
        };
        let load = venue_probes::series_promis_load(
            &world.rpc.url(world.validators.primary_port()),
            intex_nft(world),
            *id,
        )
        .expect("series Promis load");
        Terms {
            entry_price: U256::from(prices(world, *id).0),
            load: U256::from(load) * U256::from(*units),
        }
    }

    fn assert_issued(&self, world: &World) {
        let url = world.rpc.url(world.validators.primary_port());
        let nft = intex_nft(world);
        let first = prices(world, paid_series(world)[0]);
        assert_eq!(first.0, ENTRY_PRICE_MINOR);
        for series in all_series(world) {
            assert_eq!(
                venue_probes::series_state(&url, nft, series),
                Some(ISSUED),
                "series {series} was not born Issued"
            );
            assert!(!is_qualified(&url, series));
            assert_eq!(
                prices(world, series),
                first,
                "every series must share one set of terms"
            );
            assert_eq!(
                venue_probes::series_promis_load(&url, nft, series),
                Some(PROMIS_LOAD_MINOR)
            );
            assert_eq!(
                venue_probes::series_issued_count(&url, nft, series),
                Some(UNITS)
            );
        }
    }

    fn qualified(&self, world: &World) -> bool {
        let url = world.rpc.url(world.validators.primary_port());
        all_series(world)
            .into_iter()
            .all(|series| is_qualified(&url, series) && public_state(&url, series) == QUALIFIED)
    }

    /// Each paid series is paid in two parts: what is home once it qualifies, and the
    /// rest once it is called and brought home.
    fn targets(&self, world: &World, phase: Phase) -> [Target; 2] {
        let [myr, eur] = paid_series(world);
        match phase {
            Phase::Qualified => [
                part(eur, EUR_ISO, QUALIFIED_UNITS),
                part(myr, MYR_ISO, QUALIFIED_UNITS),
            ],
            Phase::Called => [
                part(myr, MYR_ISO, CALLED_UNITS),
                part(eur, EUR_ISO, CALLED_UNITS),
            ],
        }
    }

    fn called(&self, world: &World) -> bool {
        let url = world.rpc.url(world.validators.primary_port());
        let nft = intex_nft(world);
        all_series(world)
            .into_iter()
            .all(|series| venue_probes::series_state(&url, nft, series) == Some(CALLED))
            && venue_probes::series_call_deadline(&url, nft, paid_series(world)[0])
                .is_some_and(|deadline| head_time(world) <= deadline)
    }

    fn assert_paid_settled(&self, world: &World) {
        let url = world.rpc.url(world.validators.primary_port());
        let nft = intex_nft(world);
        for series in paid_series(world) {
            let paid: u32 = world
                .state
                .entity_lifecycle
                .payments
                .iter()
                .filter_map(|payment| match payment.target.item {
                    Item::Series { id, units } if id == series => Some(units),
                    _ => None,
                })
                .sum();
            assert_eq!(
                venue_probes::series_balances(&url, nft, series, owner()),
                Some((0, u64::from(paid))),
                "series {series} does not hold exactly its paid units, all settled, at home"
            );
        }
    }

    /// Waiting past the notice is the only way to reach expiry: the deadline is
    /// derived against the clock, and neither side writes anything when it passes.
    fn lapse_notice(&self, world: &mut World) {
        let port = world.validators.primary_port();
        let url = world.rpc.url(port);
        let nft = intex_nft(world);
        let [series, _] = expiring_series(world);

        let height = eth::block_number(&url).expect("head before the notice lapses");
        let pool = eth::read_call_at(
            &url,
            addresses::PROMIS_LIMIT_ADDR,
            &eth::IPromisLimit::totalUnallocatedCall {},
            height,
        )
        .expect("unallocated pool before the forfeit");
        world.state.entity_lifecycle.pool_before_forfeit = Some((height, pool));

        let deadline = venue_probes::series_call_deadline(&url, nft, series)
            .expect("the expiring series carries a call deadline");
        // A notice measured in days means the DEV profile never took, and the wait below
        // would sit out the whole run for no reason anyone could see.
        let notice = deadline.saturating_sub(u64::from(
            venue_probes::series_called_at(&url, nft, series).expect("the series was Called"),
        ));
        assert!(
            notice <= EXPIRY_BUCKET_SECS,
            "call notice is {notice}s: the DEV parameter profile is not active, so this \
             scenario would wait out the production window"
        );
        assert!(
            deadline > 0,
            "series {series} has no deadline, so it was never Called"
        );
        wait_for_chain_time(world, port, deadline + EXPIRY_MARGIN_SECS);

        // The sweep opens an expiry bucket once its hour has closed: re-queue the group
        // behind a closed one.
        test_issuance::close_call_notice(
            &url,
            DEPLOYER_KEY,
            USD_ISO,
            world
                .state
                .lifecycle_day
                .expect("the lifecycle series were issued into a day"),
            head_time(world).saturating_sub(EXPIRY_BUCKET_SECS),
        )
        .expect("close the expiry bucket the group sits in");
    }

    /// One series was settled in part and one was never touched, so the credit owed
    /// is the sum of what each still carries unrealized - never either tirage alone.
    fn assert_forfeited(&self, world: &World) {
        let port = world.validators.primary_port();
        let url = world.rpc.url(port);
        let nft = intex_nft(world);
        let target_url = target_rpc_url(world);
        let target_nft = target_intex_nft(world);
        let owner = owner();
        let (from, before) = world
            .state
            .entity_lifecycle
            .pool_before_forfeit
            .expect("the pool was read before the notice lapsed");

        let mut expired = Vec::new();
        for (series, settled_units) in expiring_series(world)
            .into_iter()
            .zip([EXPIRING_SETTLED_UNITS, 0])
        {
            let load = venue_probes::series_promis_load(&url, nft, series)
                .expect("the expiring series carries a PROMIS load");
            // Measured against the series' whole tirage, not one chain's balance: the
            // units live on both chains, and only committee-side ones could be settled.
            let tirage = venue_probes::series_issued_count(&url, nft, series)
                .expect("the expiring series carries an issued count");
            let unrealized = tirage
                .checked_sub(settled_units)
                .expect("the expiring series was issued with more units than it settled");
            let (committee_issued, committee_settled) =
                venue_probes::series_balances(&url, nft, series, owner)
                    .expect("read what the owner still holds of the expiring series");
            let (target_issued, target_settled) =
                venue_probes::series_balances(&target_url, target_nft, series, owner)
                    .expect("read what the owner still holds on the target chain");
            assert_eq!(
                committee_settled + target_settled,
                u64::from(settled_units),
                "series {series} did not end with exactly the settled part this asserts"
            );
            assert_eq!(
                committee_issued + target_issued,
                u64::from(unrealized),
                "the owner carries {} unrealized units of {series}, not {unrealized}: the \
                 rest was settled or parked, and the forfeit is not what this asserts",
                committee_issued + target_issued
            );
            assert!(
                unrealized > 0,
                "series {series} held nothing at the deadline, so the forfeit proves nothing"
            );
            expired.push((
                series,
                unrealized,
                U256::from(load) * U256::from(unrealized),
            ));
        }
        let want = before
            + expired
                .iter()
                .map(|(_, _, returned)| *returned)
                .sum::<U256>();

        poll_until(
            FORFEIT_TIMEOUT,
            || format!("unallocated PROMIS never reached {want} after the notice lapsed"),
            || world.rpc.promis_limit_total_unallocated_on(port) == Some(want),
        );
        let height = finalized_checkpoint(world).height;
        for (series, unrealized, returned) in expired {
            assert_single_event(
                &url,
                addresses::INTEX_FACTORY_ADDR,
                from,
                height,
                eth::IIntexFactory::SeriesExpired {
                    seriesId: series,
                    forfeitedUnits: unrealized,
                    returnedPromisMinor: returned,
                },
            );
        }
        assert_eq!(
            eth::read_call_at(
                &url,
                addresses::PROMIS_LIMIT_ADDR,
                &eth::IPromisLimit::totalUnallocatedCall {},
                height,
            ),
            Some(want),
            "the forfeit did not return exactly the unrealized load to the unallocated pool"
        );
    }

    /// The owner mines both paid series whole, into one Promis balance.
    fn mine_paid(&self, world: &World) -> Vec<Mined> {
        let url = world.rpc.url(world.validators.primary_port());
        let nft = intex_nft(world);
        let owner = owner();
        let before = redeem::balance(world, Ledger::Promis, DEPLOYER_KEY, owner);
        let mut amount = U256::ZERO;
        for series in paid_series(world) {
            assert_eq!(
                venue_probes::series_balances(&url, nft, series, owner).map(|(_, settled)| settled),
                Some(u64::from(UNITS)),
                "series {series} must have every unit settled"
            );
            let promis = U256::from(PROMIS_LOAD_MINOR) * U256::from(UNITS);
            let (mac, op_nonce) =
                mint_authorization(world, Ledger::Promis, DEPLOYER_KEY, owner, promis);
            let nonce = test_issuance::mine_nonce(owner, promis, series, 0)
                .expect("a nonce clearing one leading zero byte");
            test_issuance::mine_promis(&url, DEPLOYER_KEY, series, UNITS, nonce, mac.0, op_nonce)
                .expect("mine Promis from the settled units");
            // The engine counts what it burned, so the series' classes stay disjoint.
            let counts = eth::read_call(
                &url,
                addresses::INTEX_FACTORY_ADDR,
                &eth::IIntexFactory::seriesUnitCountsCall { seriesId: series },
            )
            .expect("series unit counts");
            assert_eq!(
                (counts.exercisedUnits, counts.settledUnits),
                (UNITS, 0),
                "series {series} did not count its settled units as exercised"
            );
            assert_eq!(
                venue_probes::series_balances(&url, nft, series, owner),
                Some((0, 0)),
                "mining did not burn the settled units of {series}"
            );
            amount += promis;
        }
        vec![Mined {
            owner,
            owner_key: DEPLOYER_KEY.to_owned(),
            ledger: Ledger::Promis,
            before,
            amount,
        }]
    }
}

/// Part settled and part not, so the sweep has to return the load of the unrealized
/// units alone rather than the tirage the series was issued with.
#[when("the owner settles part of one series they let run out")]
fn settle_part_of_expiring(world: &mut World) {
    let [series, _] = expiring_series(world);
    let url = world.rpc.url(world.validators.primary_port());
    // Only the units that stayed on the committee can be settled: nothing brings this
    // series home, and the rest expire where they are.
    let issued = venue_probes::series_balances(&url, intex_nft(world), series, owner())
        .expect("read what the owner holds of the expiring series")
        .0;
    assert!(
        issued > u64::from(EXPIRING_SETTLED_UNITS),
        "series {series} holds {issued} units here, too few to settle part and leave \
         the rest to run out"
    );
    let item = Item::Series {
        id: series,
        units: EXPIRING_SETTLED_UNITS,
    };
    let terms = IntexLifecycle.terms(world, &item);
    let paid = payment::pay(
        world,
        &Target {
            item,
            owner: owner(),
            owner_key: DEPLOYER_KEY.to_owned(),
            issuance_currency: GBP_ISO,
        },
        Rail::PayNote,
        USD_ISO,
        terms,
    );
    payment::assert_payments_settled(world, &[paid]);
}

#[when("a relay carries messages between the two chains")]
fn start_relay(world: &mut World) {
    let port = world.validators.primary_port();
    let committee = RelayEnd {
        url: world.rpc.url(port),
        mailbox: world
            .state
            .origin_contracts
            .as_ref()
            .expect("the Intex engine was deployed")
            .mailbox,
        domain: u32::try_from(world.rpc.chain_id(port).expect("committee chain id"))
            .expect("committee chain id fits a uint32"),
    };
    let target = RelayEnd {
        url: world
            .target_chain
            .rpc_url()
            .expect("target chain is running"),
        mailbox: world
            .state
            .target_contracts
            .as_ref()
            .expect("the Intex venue was deployed on the target chain")
            .mailbox,
        domain: u32::try_from(world.target_chain.chain_id())
            .expect("target chain id fits a uint32"),
    };

    // The NFT bridges have to know each other before either can quote a hop; nothing
    // in the deploy scripts pairs them, so the scenario that uses both does it.
    let committee_bridge = world
        .state
        .origin_contracts
        .as_ref()
        .expect("the Intex engine was deployed")
        .nft_bridge;
    let target_bridge = world
        .state
        .target_contracts
        .as_ref()
        .expect("the Intex venue was deployed on the target chain")
        .nft_bridge;
    test_issuance::set_remote_messenger(
        &committee.url,
        DEPLOYER_KEY,
        committee_bridge,
        world.target_chain.chain_id(),
        target_bridge,
    )
    .expect("point the committee bridge at the target chain");
    test_issuance::set_remote_messenger(
        &target.url,
        DEPLOYER_KEY,
        target_bridge,
        u64::from(committee.domain),
        committee_bridge,
    )
    .expect("point the target bridge home");

    world.relay = Some(Relay::start(committee, target, DEPLOYER_KEY.to_owned()));
}

/// The day the chain is in, taken from its own head rather than the host clock.
fn chain_worldwide_day_offset(world: &World, port: u16, offset_secs: i64) -> u32 {
    let timestamp = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp");
    outbe_primitives::time::worldwide_day_from_timestamp(
        timestamp.saturating_add_signed(offset_secs),
    )
}

#[when("the owner brings part of the target-chain units home")]
fn bridge_part_home(world: &mut World) {
    bring_home(world, TRADABLE_HOP_UNITS);
}

#[when("the owner brings the remaining units home to their own address in one batch")]
fn bridge_rest_home(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let target_url = world
        .target_chain
        .rpc_url()
        .expect("target chain is running");
    let nft = intex_nft(world);
    let bridge = world
        .state
        .target_contracts
        .as_ref()
        .expect("the Intex venue was deployed on the target chain")
        .nft_bridge;
    let owner = crate::world::origin_venue::deployer_address();
    let home_chain = u32::try_from(world.rpc.chain_id(port).expect("committee chain id"))
        .expect("fits a uint32");
    let amount = TARGET_UNITS - TRADABLE_HOP_UNITS;

    // An owner with more than one series moves them together, so this hop takes the
    // batch route the first one did not: one burn set here, one mint set at home.
    let tokens: Vec<(alloy_primitives::U256, u32)> = world
        .state
        .lifecycle_series
        .iter()
        .map(|series| {
            (
                venue_probes::issued_token_id(&url, nft, *series).expect("issued token id"),
                amount,
            )
        })
        .collect();
    let before: Vec<u64> = world
        .state
        .lifecycle_series
        .iter()
        .map(|series| {
            venue_probes::series_balances(&url, nft, *series, owner)
                .expect("series balances at home")
                .0
        })
        .collect();

    test_issuance::batch_bridge_home(
        &target_url,
        DEPLOYER_KEY,
        bridge,
        home_chain,
        owner,
        &tokens,
    )
    .expect("send the remaining units home in one batch");

    let deadline = Instant::now() + Duration::from_secs(DELIVERY_TIMEOUT_SECS);
    for (series, before) in world.state.lifecycle_series.clone().into_iter().zip(before) {
        let want = before + u64::from(amount);
        loop {
            if venue_probes::series_balances(&url, nft, series, owner)
                .is_some_and(|(issued, _)| issued >= want)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "series {series} never arrived home in the batch hop"
            );
            sleep(Duration::from_secs(2));
        }
    }
}

/// Drive the owner's own bridge hop for every series and wait for the units to land.
fn bring_home(world: &mut World, amount: u32) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let target_url = world
        .target_chain
        .rpc_url()
        .expect("target chain is running");
    let nft = intex_nft(world);
    let bridge = world
        .state
        .target_contracts
        .as_ref()
        .expect("the Intex venue was deployed on the target chain")
        .nft_bridge;
    let owner = crate::world::origin_venue::deployer_address();
    let home_chain = u32::try_from(world.rpc.chain_id(port).expect("committee chain id"))
        .expect("fits a uint32");

    for series in world.state.lifecycle_series.clone() {
        let before = venue_probes::series_balances(&url, nft, series, owner)
            .expect("series balances at home")
            .0;
        let token_id = venue_probes::issued_token_id(&url, nft, series).expect("issued token id");

        test_issuance::bridge_home(
            &target_url,
            DEPLOYER_KEY,
            bridge,
            home_chain,
            token_id,
            owner,
            amount,
        )
        .expect("send the units home");

        let want = before + u64::from(amount);
        let deadline = Instant::now() + Duration::from_secs(DELIVERY_TIMEOUT_SECS);
        loop {
            if venue_probes::series_balances(&url, nft, series, owner)
                .is_some_and(|(issued, _)| issued >= want)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "series {series} units never arrived home; the relay carried nothing back"
            );
            sleep(Duration::from_secs(2));
        }
    }
}

#[then("the series left to run out read Expired on both chains")]
fn unsettled_series_expired(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let target_url = target_rpc_url(world);
    let nft = intex_nft(world);
    let target_nft = target_intex_nft(world);
    // No message carries expiry across: each chain derives it from the same calledAt
    // and notice, so both have to agree on their own.
    let target_router = world
        .state
        .target_contracts
        .as_ref()
        .expect("the Intex venue was deployed on the target chain")
        .target_router;

    // Anvil does not mine while this step only reads. Advance its clock even when
    // the Called mark was already applied and there is nothing left to retry.
    let now = eth::latest_block_timestamp(&url).expect("committee head timestamp");
    world
        .target_chain
        .sync_clock_to(now)
        .expect("carry the elapsed call notice to the target chain");

    for series in expiring_series(world) {
        // A mark whose calledAt sits ahead of this chain's clock is parked, not
        // applied, and nothing in a localnet plays the operator who retries it.
        let parked = eth::read_call(
            &target_url,
            target_router,
            &venue_probes::IIssuedSeries::parkedMarkCall { seriesId: series },
        );
        if parked.is_some_and(|called_at| called_at != 0) {
            eth::send_call(
                &target_url,
                target_router,
                crate::world::forge::DEPLOYER_KEY,
                &venue_probes::IIssuedSeries::applyParkedMarkCall { seriesId: series },
                None,
            )
            .expect("apply the mark the target chain parked");
        }

        for (label, at, collection) in [
            ("committee", url.as_str(), nft),
            ("target chain", target_url.as_str(), target_nft),
        ] {
            let deadline = Instant::now() + Duration::from_secs(DELIVERY_TIMEOUT_SECS);
            loop {
                if venue_probes::series_state(at, collection, series) == Some(EXPIRED) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "series {series} never read Expired on the {label}: {:?}; the call time parked \
                     on the target router reads {:?}",
                    venue_probes::series_state(at, collection, series),
                    eth::read_call(
                        &target_url,
                        target_router,
                        &venue_probes::IIssuedSeries::parkedMarkCall { seriesId: series },
                    )
                );
                sleep(Duration::from_secs(2));
            }
        }
        let committee_deadline = venue_probes::series_call_deadline(&url, nft, series)
            .expect("committee expiry deadline");
        let target_deadline = venue_probes::series_call_deadline(&target_url, target_nft, series)
            .expect("target expiry deadline");
        let target_timestamp =
            eth::latest_block_timestamp(&target_url).expect("target expiry observation timestamp");
        assert_eq!(
            target_deadline, committee_deadline,
            "series {series} expiry deadline parity"
        );
        assert!(
            target_timestamp > target_deadline,
            "series {series} target clock did not pass expiry"
        );
        eprintln!(
            "INTEX_EXPIRY_CLOCK series={series} committee_timestamp={now} \
             target_timestamp={target_timestamp} deadline={target_deadline} pending_mark={parked:?}"
        );
    }
}

/// The two series left to run out: one settled in part, one never touched.
fn expiring_series(world: &World) -> [FixedBytes<14>; 2] {
    [
        world
            .state
            .expiring_series
            .expect("a series was issued to be settled in part"),
        world
            .state
            .untouched_series
            .expect("a series was issued to be left untouched"),
    ]
}

/// The two series paid for whole: issued in MYR, and in EUR.
fn paid_series(world: &World) -> [FixedBytes<14>; 2] {
    let [myr, eur] = world.state.lifecycle_series[..] else {
        panic!("the scenario pays for two series")
    };
    [myr, eur]
}

fn all_series(world: &World) -> [FixedBytes<14>; 4] {
    let [myr, eur] = paid_series(world);
    let [expiring, untouched] = expiring_series(world);
    [myr, eur, expiring, untouched]
}

fn part(series: FixedBytes<14>, issuance_currency: u16, units: u32) -> Target {
    Target {
        item: Item::Series { id: series, units },
        owner: owner(),
        owner_key: DEPLOYER_KEY.to_owned(),
        issuance_currency,
    }
}

fn owner() -> Address {
    crate::world::origin_venue::deployer_address()
}

fn prices(world: &World, series: FixedBytes<14>) -> (u64, u64, u64) {
    venue_probes::series_prices(
        &world.rpc.url(world.validators.primary_port()),
        intex_nft(world),
        series,
    )
    .expect("series prices")
}

fn public_state(url: &str, series: FixedBytes<14>) -> u8 {
    eth::read_call(
        url,
        addresses::INTEX_ADDR,
        &eth::IIntex::seriesDataCall { seriesId: series },
    )
    .unwrap_or_else(|| panic!("series {series} does not read back"))
    .state
}

fn is_qualified(url: &str, series: FixedBytes<14>) -> bool {
    eth::read_call(
        url,
        addresses::INTEX_FACTORY_ADDR,
        &eth::IIntexFactory::isSeriesQualifiedCall { seriesId: series },
    )
    .unwrap_or_else(|| panic!("series {series} qualification does not read back"))
}
