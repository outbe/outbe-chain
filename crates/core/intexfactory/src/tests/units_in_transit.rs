//! OIP-00416, acceptance case A18.10: Intex units in transit between two chains when their
//! series forfeits. A model of the two `IntexNFT1155Bridge` ends tracks the units on the
//! source chain, on the destination chain and in the kept failed mints. The Outbe chain
//! keeps the totals of the series and returns the forfeited load to the Promis Limit.

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_intex::{api, IntexContract, IntexState, SeriesId};
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_promislimit::precompile::IPromisLimit;
use outbe_promislimit::{LimitSource, PromisLimitContract};
use outbe_system_common::addresses::PROMIS_LIMIT_ADDRESS;
use outbe_vaultrouter::settlement::SettlementPayment;

use super::{SeedCalledGroup, CHAIN_ID, DAY, ISSUED_AT, PROMIS_LOAD_MINOR};
use crate::errors::IntexFactoryError;
use crate::runtime::{self, OwnerUnits};
use crate::schema::IssuanceParams;

const SERIES_DAY: u32 = 7;
const ISSUED_UNITS: u32 = 12;
/// The issued units less 2 settled, 1 mined and 2 sent to the Gem Factory before the call.
const UNSETTLED: u32 = 7;
/// The settlement deadline of the called group.
const DEADLINE: u64 = ISSUED_AT + 7 * DAY;
/// The bridge steps before the deadline.
const IN_TIME: u64 = ISSUED_AT + DAY;

/// The holder of the units on the source chain and the recipient of the bridge message.
fn holder() -> Address {
    Address::repeat_byte(0x11)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chain {
    Source,
    Destination,
}

impl Chain {
    const fn other(self) -> Self {
        match self {
            Self::Source => Self::Destination,
            Self::Destination => Self::Source,
        }
    }
}

/// The reverts of the model, with the names that `IntexNFT1155` and its bridge use.
#[derive(Debug, PartialEq, Eq)]
enum BridgeError {
    NonexistentToken,
    BridgeAfterDeadline,
    NoSuchFailedCrosschainMint,
}

/// A failed mint that a bridge keeps for `retryCrosschainMint` or `reclaimToSource`.
#[derive(Clone, Copy, Debug)]
struct FailedMint {
    chain: Chain,
    units: u32,
}

/// The two bridge ends of one called series. One step is one bridge call and the delivery
/// of the message that it sends.
#[derive(Clone, Debug)]
struct Bridges {
    /// The transit close of the series. After it, no chain mints the series.
    transit_close: u64,
    /// Until the destination knows the series, its mint fails with `NonexistentToken`.
    destination_has_series: bool,
    held_on_source: u32,
    held_on_destination: u32,
    burned_on_source: u32,
    minted_on_destination: u32,
    minted_back_on_source: u32,
    failed_mints: BTreeMap<u32, FailedMint>,
    next_receive_id: u32,
}

impl Bridges {
    fn new(held_on_source: u32, transit_close: u64) -> Self {
        Self {
            transit_close,
            destination_has_series: false,
            held_on_source,
            held_on_destination: 0,
            burned_on_source: 0,
            minted_on_destination: 0,
            minted_back_on_source: 0,
            failed_mints: BTreeMap::new(),
            next_receive_id: 0,
        }
    }

    /// `crosschainMint` on `chain`.
    fn mint(&mut self, chain: Chain, units: u32, now: u64) -> Result<(), BridgeError> {
        if chain == Chain::Destination && !self.destination_has_series {
            return Err(BridgeError::NonexistentToken);
        }
        if now > self.transit_close {
            return Err(BridgeError::BridgeAfterDeadline);
        }
        match chain {
            Chain::Source => {
                self.held_on_source += units;
                self.minted_back_on_source += units;
            }
            Chain::Destination => {
                self.held_on_destination += units;
                self.minted_on_destination += units;
            }
        }
        Ok(())
    }

    /// `receiveMessage` on `chain`. Returns the receive id of a failed mint that it keeps.
    fn deliver(&mut self, chain: Chain, units: u32, now: u64) -> Option<u32> {
        self.mint(chain, units, now).err()?;
        let receive_id = self.next_receive_id;
        self.next_receive_id += 1;
        self.failed_mints
            .insert(receive_id, FailedMint { chain, units });
        Some(receive_id)
    }

    /// `crosschainBurn` on the source and the delivery to the destination.
    fn send(&mut self, units: u32, now: u64) -> Option<u32> {
        self.held_on_source -= units;
        self.burned_on_source += units;
        self.deliver(Chain::Destination, units, now)
    }

    /// `retryCrosschainMint`. A failed mint reverts the call, so the bridge keeps the entry.
    fn retry(&mut self, receive_id: u32, now: u64) -> Result<(), BridgeError> {
        let failed = *self
            .failed_mints
            .get(&receive_id)
            .ok_or(BridgeError::NoSuchFailedCrosschainMint)?;
        self.mint(failed.chain, failed.units, now)?;
        self.failed_mints.remove(&receive_id);
        Ok(())
    }

    /// `reclaimToSource`: deletes the entry and sends its units back to the chain they came from.
    fn reclaim(&mut self, receive_id: u32, now: u64) -> Result<Option<u32>, BridgeError> {
        let failed = self
            .failed_mints
            .remove(&receive_id)
            .ok_or(BridgeError::NoSuchFailedCrosschainMint)?;
        Ok(self.deliver(failed.chain.other(), failed.units, now))
    }

    fn failed_units(&self) -> u32 {
        self.failed_mints.values().map(|failed| failed.units).sum()
    }

    /// The units on both chains and in the kept failed mints.
    fn live_units(&self) -> u32 {
        self.held_on_source + self.held_on_destination + self.failed_units()
    }

    /// Item 4 of OIP-00416: the burned units equal the minted units, the units of the kept
    /// failed mints and the units that a reclaim minted back.
    fn conserved(&self) -> Result<(), String> {
        let accounted =
            self.minted_on_destination + self.failed_units() + self.minted_back_on_source;
        if self.burned_on_source == accounted {
            Ok(())
        } else {
            Err(format!(
                "burned {} != minted {} + failed {} + minted back {}",
                self.burned_on_source,
                self.minted_on_destination,
                self.failed_units(),
                self.minted_back_on_source
            ))
        }
    }
}

/// The paths of item 3 of OIP-00416. Each path starts with a failed mint of k units.
#[derive(Clone, Copy, Debug)]
enum Path {
    /// a. The destination mints the k units before the deadline.
    RetryInTime,
    /// b. The source mints the k units again before the deadline.
    ReclaimInTime,
    /// c. No retry and no reclaim. The series forfeits after its transit close.
    Forfeit,
    /// d. As c, then a retry after the forfeit.
    RetryAfterForfeit,
    /// e. As c, then a reclaim after the forfeit.
    ReclaimAfterForfeit,
}

const PATHS: [Path; 5] = [
    Path::RetryInTime,
    Path::ReclaimInTime,
    Path::Forfeit,
    Path::RetryAfterForfeit,
    Path::ReclaimAfterForfeit,
];

/// The Outbe chain with one called series.
struct Outbe {
    storage: HashMapStorageProvider,
    series_id: SeriesId,
}

impl Outbe {
    /// Issues the series, realizes 5 of its units and calls its group on `DEADLINE`.
    fn called_series() -> Self {
        let mut storage = super::factory_provider();
        let series_id = super::sid(SERIES_DAY);
        StorageHandle::enter(&mut storage, |s| {
            super::select_prod_profile(&s);
            runtime::issue_intex(
                &s,
                IssuanceParams {
                    issued_units: ISSUED_UNITS,
                    ..super::sample(SERIES_DAY)
                },
            )
            .unwrap();
            api::record_settled_units(&s, series_id, holder(), 3).unwrap();
            api::record_mined_units(&s, series_id, holder(), 1).unwrap();
            api::record_gem_factory_units(&s, series_id, holder(), 2).unwrap();
            IntexContract::new(s.clone())
                .seed_called_group(series_id, DEADLINE)
                .unwrap();
        });
        Self { storage, series_id }
    }

    fn view<R>(&mut self, now: u64, f: impl FnOnce(&StorageHandle<'_>, SeriesId) -> R) -> R {
        let series_id = self.series_id;
        self.storage.set_timestamp(U256::from(now));
        StorageHandle::enter(&mut self.storage, |s| f(&s, series_id))
    }

    fn transit_close(&mut self) -> u64 {
        self.view(ISSUED_AT, |s, id| {
            IntexContract::new(s.clone())
                .called_deadline
                .read(&super::group_of(s, id))
                .unwrap()
        })
    }

    /// The daily pass of the IntexFactory: the call sweep and the forfeit of the called groups.
    fn daily_pass(&mut self, now: u64) {
        self.view(now, |s, _| {
            let ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_number: 2,
                    timestamp: now,
                    chain_id: CHAIN_ID,
                    genesis_hash: B256::ZERO,
                    genesis_timestamp: DEADLINE - DAY,
                    proposer: Address::ZERO,
                    validators: Vec::new(),
                    block_gas_limit: 0,
                    user_gas_used: 0,
                },
                s.clone(),
            );
            super::called::daily_pass(&ctx).unwrap();
        });
    }

    /// The amounts of the Promis Limit credits with the source `LimitSource::Intex`.
    fn intex_credits(&self) -> Vec<U256> {
        self.storage
            .get_events(PROMIS_LIMIT_ADDRESS)
            .iter()
            .filter_map(|log| IPromisLimit::PromisLimitAdded::decode_log_data(log).ok())
            .filter(|added| added.source == LimitSource::Intex as u8)
            .map(|added| added.amountMinor)
            .collect()
    }

    /// Items 4 to 6 of OIP-00416 after one step.
    fn check(&mut self, bridges: &Bridges, now: u64, step: &str) {
        bridges
            .conserved()
            .unwrap_or_else(|gap| panic!("{step}: {gap}"));
        let (unsettled, limit, state) = self.view(now, |s, id| {
            let record = api::get_series(s, id).unwrap();
            (
                api::forfeitable_units(s, &record).unwrap().units,
                PromisLimitContract::new(s.clone()).promis_limit().unwrap(),
                api::state(s, &record).unwrap(),
            )
        });
        // The units in transit count one time in the totals of the series.
        assert_eq!(unsettled, UNSETTLED, "{step}");
        assert_eq!(bridges.live_units(), unsettled, "{step}");
        let credit = U256::from(PROMIS_LOAD_MINOR) * U256::from(unsettled);
        if state == IntexState::Forfeited {
            assert_eq!(self.intex_credits(), vec![credit], "{step}");
            assert_eq!(limit, credit, "{step}");
        } else {
            assert!(self.intex_credits().is_empty(), "{step}");
            assert_eq!(limit, U256::ZERO, "{step}");
        }
    }

    /// Item 7 of OIP-00416: the units of a late mint cannot be settled and cannot go to
    /// the Gem Factory.
    fn late_units_give_no_value(&mut self, units: u32, now: u64) {
        self.view(now, |s, id| {
            let settle = runtime::settle_intex(
                s,
                OwnerUnits {
                    series_id: id,
                    owner: holder(),
                    units: U256::from(units),
                },
                SettlementPayment {
                    payer: holder(),
                    asset: super::settlement_asset(),
                    snapshot_id: U256::ZERO,
                },
                super::amount_cap(),
            );
            assert_eq!(
                settle.unwrap_err().to_string(),
                PrecompileError::from(IntexFactoryError::SettlementDeadlinePassed).to_string()
            );
            let position = outbe_gemfactory::api::create_gem_factory_position(
                s,
                holder(),
                id,
                U256::from(units),
            );
            assert_eq!(
                position.unwrap_err().to_string(),
                PrecompileError::from(
                    outbe_gemfactory::errors::GemFactoryError::SourceIntexNotIssued
                )
                .to_string()
            );
        });
    }
}

/// Moves `k` units to a destination that does not know the series yet, then follows `path`.
fn follow(path: Path, k: u32) {
    let mut outbe = Outbe::called_series();
    let transit_close = outbe.transit_close();
    let mut bridges = Bridges::new(UNSETTLED, transit_close);
    outbe.check(&bridges, IN_TIME, "start");

    let receive_id = bridges
        .send(k, IN_TIME)
        .expect("the destination keeps the failed mint");
    assert_eq!((bridges.burned_on_source, bridges.failed_units()), (k, k));
    outbe.check(&bridges, IN_TIME, "failed mint");

    // Item 8: a kept failed mint deleted with no mint and no reclaim breaks the balance.
    let mut faulty = bridges.clone();
    faulty.failed_mints.remove(&receive_id);
    assert!(faulty.conserved().is_err());

    match path {
        Path::RetryInTime => {
            bridges.destination_has_series = true;
            bridges.retry(receive_id, IN_TIME + 1).unwrap();
            assert_eq!(bridges.minted_on_destination, k);
        }
        Path::ReclaimInTime => {
            assert_eq!(bridges.reclaim(receive_id, IN_TIME + 1), Ok(None));
            assert_eq!(bridges.minted_back_on_source, k);
        }
        Path::Forfeit | Path::RetryAfterForfeit | Path::ReclaimAfterForfeit => {}
    }
    outbe.check(&bridges, IN_TIME + 1, "in time");

    // The series stays called through the transit close.
    outbe.daily_pass(DEADLINE);
    outbe.check(&bridges, DEADLINE, "deadline");
    outbe.check(&bridges, transit_close, "transit close");
    assert_eq!(
        outbe.view(transit_close, super::series_state),
        IntexState::Called
    );

    let due = IntexContract::day_end(IntexContract::deadline_day(transit_close));
    outbe.daily_pass(due);
    assert_eq!(outbe.view(due, super::series_state), IntexState::Forfeited);
    outbe.check(&bridges, due, "forfeit");
    assert!(
        outbe.intex_credits()[0] < U256::from(PROMIS_LOAD_MINOR) * U256::from(ISSUED_UNITS),
        "the realized units return no load"
    );

    let late = due + 1;
    match path {
        Path::RetryAfterForfeit => {
            bridges.destination_has_series = true;
            assert_eq!(
                bridges.retry(receive_id, late),
                Err(BridgeError::BridgeAfterDeadline)
            );
            assert_eq!(bridges.failed_units(), k);
            outbe.late_units_give_no_value(k, late);
        }
        Path::ReclaimAfterForfeit => {
            let parked = bridges.reclaim(receive_id, late).unwrap();
            let parked = parked.expect("the late reclaim parks on the source");
            assert_eq!(bridges.failed_mints[&parked].chain, Chain::Source);
            assert_eq!(bridges.minted_back_on_source, 0);
            assert_eq!(
                bridges.reclaim(receive_id, late),
                Err(BridgeError::NoSuchFailedCrosschainMint)
            );
            outbe.late_units_give_no_value(k, late);
        }
        Path::Forfeit => assert_eq!(bridges.failed_units(), k),
        Path::RetryInTime | Path::ReclaimInTime => assert_eq!(bridges.failed_units(), 0),
    }
    outbe.check(&bridges, late, "late step");

    // A second pass credits nothing.
    outbe.daily_pass(due + DAY);
    outbe.check(&bridges, due + DAY, "second pass");
}

#[test]
fn intex_units_in_transit_keep_the_units_and_the_credit() {
    for k in [UNSETTLED, 3] {
        for path in PATHS {
            follow(path, k);
        }
    }
}
