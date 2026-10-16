use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_intex::SeriesId;
use outbe_oracle::api::AddressPair;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::math::constants::REAL_ID_SHIFT;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::time::SECONDS_PER_DAY_U32;
use outbe_primitives::time::{date_key_to_utc_timestamp, previous_date_key, timestamp_to_date_key};

use crate::called;
use crate::constants::{MAX_RECIPIENTS_PER_ISSUANCE, MAX_SERIES_PER_MESSAGE};
use crate::precompile::{self, IIntexFactory};
use crate::runtime;
use crate::schema::{IntexFactoryContract, IssuanceParams};
use outbe_intex::constants::{CALL_RATE_PCT, CALL_THRESHOLD, CALL_WINDOW};
use outbe_intex::{Group, IntexContract};

/// ISO code every fixture prices in.
const REFERENCE_ISO: u16 = 840;
use outbe_primitives::time::SECONDS_PER_DAY as DAY;

fn owner() -> Address {
    address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
}

fn settlement_asset() -> Address {
    address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC")
}

const CHAIN_ID: u64 = 1;
const ISSUED_AT: u32 = 1_700_000_000;
const PROMIS_LOAD_MINOR: u128 = 1_000_000; // 1 PROMIS in PROMIS-unit
const CALL_NOTICE_PERIOD: u32 = 7 * SECONDS_PER_DAY_U32;

// COEN clearing price and the trigger derived from it at issuance.
const ENTRY_PRICE: u64 = 1_000_000;
const EXPECTED_TRIGGER: u64 = 2_280_000; // ENTRY_PRICE * 228/100

fn with_factory<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = factory_provider();
    StorageHandle::enter(&mut storage, |handle| {
        select_prod_profile(&handle);
        f(handle)
    })
}

fn factory_provider() -> HashMapStorageProvider {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    // Stub IntexNFT1155: void calls succeed, and balanceOf returns 0 (32 bytes).
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    // Stub OriginRouter: send* calls return bytes32 sendId (32 bytes). The value is ignored.
    storage.stub_sub_call_at(
        crate::constants::ORIGIN_ROUTER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    storage
}

/// These cases assert the PROD terms. An unset profile resolves by chain id, and
/// the test chain is not mainnet.
fn select_prod_profile(storage: &StorageHandle<'_>) {
    outbe_intex::IntexContract::new(storage.clone())
        .config_profile
        .write(outbe_intex::config::PROFILE_PROD)
        .unwrap();
}

/// Test ids carry a fixed USD/U pair. Only the day varies.
fn sid(worldwide_day: u32) -> SeriesId {
    SeriesId::pack(WorldwideDay::new(worldwide_day), *b"USD", b'U').unwrap()
}

/// Force-call one group against the protocol call window. Returns how many series moved.
fn call_group<'s>(
    s: &StorageHandle<'s>,
    f: &mut IntexContract<'s>,
    group: &Group,
    last_closed_day: u32,
    now_ts: u64,
) -> u32 {
    let secs_per_day = SECONDS_PER_DAY_U32;
    let window = outbe_oracle::call_window::CallWindow::load(
        s,
        group.iso_code,
        last_closed_day,
        outbe_primitives::call_breach::ScanTerms {
            window_days: CALL_WINDOW / secs_per_day,
            threshold_days: CALL_THRESHOLD / secs_per_day,
        },
    )
    .unwrap();
    if window.ceiling().is_none() {
        return 0;
    }
    let factory = IntexFactoryContract::new(s.clone());
    let call = called::GroupCall {
        storage: s,
        intex: f,
        factory: &factory,
    };
    called::call_if_breached(call, group, &window, now_ts).unwrap()
}

fn sample(worldwide_day: u32) -> IssuanceParams {
    IssuanceParams {
        series_id: sid(worldwide_day),
        worldwide_day: worldwide_day.into(),
        issued_units: 100,
        promis_load_minor: PROMIS_LOAD_MINOR,
        entry_price_minor: U256::from(ENTRY_PRICE),
        issuance_currency: 840,
        reference_currency: 840,
        recipients: vec![],
        units: vec![],
        recipient_chains: vec![],
        // One target in the snapshot exercises the per-chain ISSUANCE loop (empty recipients).
        snapshot_chains: vec![1],
    }
}

/// Seeds a group the way a call leaves it: members in the group, the group queued.
trait SeedCalledGroup {
    fn seed_called_group(
        &mut self,
        iso: u16,
        day: WorldwideDay,
        deadline: u64,
        members: &[SeriesId],
    ) -> outbe_primitives::error::Result<()>;
}

impl SeedCalledGroup for IntexContract<'_> {
    fn seed_called_group(
        &mut self,
        iso: u16,
        day: WorldwideDay,
        deadline: u64,
        members: &[SeriesId],
    ) -> outbe_primitives::error::Result<()> {
        use outbe_primitives::storage::types::Storable as _;
        let key = IntexContract::scoped(iso, day.value());
        for (index, series_id) in members.iter().enumerate() {
            self.call_group_members.write(
                &IntexContract::group_member_key(iso, day, index as u32),
                series_id.to_word(),
            )?;
        }
        self.call_group_count.write(&key, members.len() as u32)?;
        self.push_called(iso, day, deadline)
    }
}

mod capacity_conservation;
mod direct_fx_admission;
mod entrypoints;
mod groups;
mod issuance;
mod lifecycle;
mod mining_after_deadline;
mod mining_sequence;
mod parked;
mod proceeds;
mod router_fault;
mod scans;
mod settlement;

// --- // --- Shared across the test modules ---
const PAIR_ID: u32 = 1;
const EUR_ISO: u16 = 978;
const EUR_PAIR_ID: u32 = 2;

fn word(value: u64) -> alloy_primitives::Bytes {
    alloy_primitives::Bytes::from(U256::from(value).to_be_bytes::<32>().to_vec())
}

/// Publish `rate` as the finalized COEN/`iso_code` VWAP of the UTC day before
/// `ISSUED_AT` and as the only observation of the trailing window settlement
/// converts at.
fn write_day_rate(oracle: &OracleContract, iso_code: u16, pair_id: u32, rate: U256) {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(iso_code);
    oracle.pair_to_index.write(&pair, pair_id).unwrap();
    let day = previous_date_key(timestamp_to_date_key(ISSUED_AT as u64));
    oracle.record_utc_day_vwap(day, pair_id, rate).unwrap();
    let snapshot = outbe_oracle::api::current_vwap_snapshot(oracle.storage.clone()).unwrap();
    OracleContract::new(oracle.storage.clone())
        .write_snapshot(
            snapshot.cutoff() - 1,
            &[(pair, rate, U256::from(1_000_000u64))],
        )
        .unwrap();
}

fn write_rate(oracle: &OracleContract, iso_code: u16, pair_id: u32, rate: U256) {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(iso_code);
    oracle.pair_to_index.write(&pair, pair_id).unwrap();
    oracle.exchange_rate.write(&pair_id, rate).unwrap();
    oracle
        .exchange_rate_timestamp
        .write(&pair_id, ISSUED_AT as u64)
        .unwrap();
}
