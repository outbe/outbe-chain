use std::time::Instant;

use alloy_primitives::{Address, Bytes, U256};
use outbe_gemfactory::GemFactoryContract;
use outbe_intex::SeriesId;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key, WorldwideDay};
use outbe_primitives::{
    addresses::INTEX_NFT1155_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    units::{SCALE_1E6_U128, SCALE_1E6_U64},
};

use super::support::{capture_execution, elapsed_ns};
use crate::{BenchmarkScenario, ExecutionClass, GasLedger, Observation, Profile, ScenarioMetadata};

const CHAIN_ID: u64 = 1;
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const T_NOW: u64 = 1_700_000_000;
const ALICE: Address = Address::repeat_byte(0x11);
const BOB: Address = Address::repeat_byte(0x22);
const PARK_UNITS: u64 = 100;

#[derive(Clone, Copy)]
enum GemPath {
    Direct,
    Position,
    Merchant,
}

pub struct GemScenario {
    path: GemPath,
}

impl GemScenario {
    #[must_use]
    pub const fn direct() -> Self {
        Self {
            path: GemPath::Direct,
        }
    }

    #[must_use]
    pub const fn position() -> Self {
        Self {
            path: GemPath::Position,
        }
    }

    #[must_use]
    pub const fn merchant() -> Self {
        Self {
            path: GemPath::Merchant,
        }
    }
}

enum PreparedGemPath {
    Direct,
    Position,
    Merchant {
        position_id: U256,
        initial_capacity: U256,
    },
}

pub struct PreparedGem {
    provider: HashMapStorageProvider,
    path: PreparedGemPath,
}

fn six_decimal_unit() -> U256 {
    U256::from(SCALE_1E6_U64)
}

fn source_intex_id() -> SeriesId {
    SeriesId::pack(WorldwideDay::new(20_260_212), *b"USD", b'U')
        .expect("benchmark Intex id is canonical")
}

fn seed_oracle(storage: StorageHandle<'_>) -> Result<(), String> {
    let rate = U256::from(2) * six_decimal_unit();
    let index = outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
        .map_err(|error| error.to_string())?;
    outbe_oracle::api::set_exchange_rate(
        storage.clone(),
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        outbe_oracle::api::RateObservation {
            rate,
            block_number: 1,
            timestamp: T_NOW,
        },
    )
    .map_err(|error| error.to_string())?;
    let oracle = OracleContract::new(storage);
    oracle
        .record_utc_day_vwap(
            previous_date_key(timestamp_to_date_key(T_NOW)).expect("date step"),
            index,
            rate,
        )
        .map_err(|error| error.to_string())?;
    oracle
        .reference_currencies
        .push(840_u16)
        .map_err(|error| error.to_string())
}

fn seed_series(storage: &StorageHandle<'_>) -> Result<(), String> {
    outbe_intex::api::create_series(
        storage,
        outbe_intex::CreateSeriesParams {
            series_id: source_intex_id(),
            worldwide_day: WorldwideDay::new(0),
            issued_units: PARK_UNITS as u32,
            promis_load_minor: SCALE_1E6_U128,
            entry_price_minor: six_decimal_unit(),
            call_price_minor: U256::ZERO,
            call_trigger: outbe_intex::IntexCallTrigger::default(),
            issued_at: T_NOW as u32,
            issuance_currency: 840,
            reference_currency: 840,
        },
    )
    .map_err(|error| error.to_string())
}

fn new_provider(needs_oracle: bool) -> Result<HashMapStorageProvider, String> {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(1);
    provider.set_timestamp(U256::from(T_NOW));
    provider.stub_sub_call_at(
        INTEX_NFT1155_ADDRESS,
        Bytes::from(U256::from(PARK_UNITS).to_be_bytes::<32>().to_vec()),
    );
    if needs_oracle {
        StorageHandle::enter(&mut provider, seed_oracle)?;
    }
    Ok(provider)
}

impl BenchmarkScenario for GemScenario {
    type Prepared = PreparedGem;

    fn metadata(&self) -> ScenarioMetadata {
        let (id, name) = match self.path {
            GemPath::Direct => (
                "gem/create/reward/single".to_owned(),
                "Reward Gem creation".to_owned(),
            ),
            GemPath::Position => (
                "gem_position/create/marginal/single".to_owned(),
                "GemPosition creation (marginal child call)".to_owned(),
            ),
            GemPath::Merchant => (
                "gem/create/merchant/single".to_owned(),
                "Merchant Gem creation".to_owned(),
            ),
        };
        ScenarioMetadata::new(
            id,
            name,
            ExecutionClass::InternalTransition,
            Profile::Single,
        )
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        match self.path {
            GemPath::Direct => {
                let provider = new_provider(true)?;
                Ok(PreparedGem {
                    provider,

                    path: PreparedGemPath::Direct,
                })
            }
            GemPath::Position => {
                let mut provider = new_provider(true)?;
                StorageHandle::enter(&mut provider, |storage| seed_series(&storage))?;
                Ok(PreparedGem {
                    provider,

                    path: PreparedGemPath::Position,
                })
            }
            GemPath::Merchant => {
                let mut provider = new_provider(true)?;
                let position_id = StorageHandle::enter(&mut provider, |storage| {
                    seed_series(&storage)?;
                    outbe_gemfactory::api::issue_gem_position(
                        &storage,
                        ALICE,
                        source_intex_id(),
                        U256::from(PARK_UNITS),
                    )
                    .map_err(|error| error.to_string())
                })?;
                Ok(PreparedGem {
                    provider,

                    path: PreparedGemPath::Merchant {
                        position_id,
                        initial_capacity: U256::from(SCALE_1E6_U64) * U256::from(PARK_UNITS),
                    },
                })
            }
        }
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        measure(prepared)
    }
}

fn measure(prepared: &PreparedGem) -> Result<Observation, String> {
    let mut provider = prepared.provider.clone();
    provider.set_gas_limit(BLOCK_GAS_LIMIT);
    provider.enable_production_storage_gas_metering();
    provider.enable_storage_trace();
    let event_offset = provider.get_ordered_events().len();
    let started = Instant::now();
    let output = StorageHandle::enter(&mut provider, |storage| match prepared.path {
        PreparedGemPath::Direct => outbe_gemfactory::api::issue_gem(
            &storage,
            outbe_gemfactory::GemIssueParams {
                owner: ALICE,
                promis_load_minor: U256::from(10) * six_decimal_unit(),
                issuance_currency: 840,
                reference_currency: 840,
                // The reward path resolves the price for the gem's own day. The
                // benchmark measures the mint, so the seeded rate stands in for it.
                entry_price_minor: U256::from(2) * six_decimal_unit(),
                call_anchor_price_minor: U256::from(2) * six_decimal_unit(),
            },
        )
        .map_err(|error| error.to_string()),
        PreparedGemPath::Position => outbe_gemfactory::api::issue_gem_position(
            &storage,
            ALICE,
            source_intex_id(),
            U256::from(PARK_UNITS),
        )
        .map_err(|error| error.to_string()),
        PreparedGemPath::Merchant { position_id, .. } => outbe_gemfactory::api::issue_merchant_gem(
            &storage,
            outbe_gemfactory::schema::MerchantGemIssueParams {
                caller: ALICE,
                position_id,
                owner: BOB,
                promis_load_minor: U256::from(10) * six_decimal_unit(),
            },
        )
        .map_err(|error| error.to_string()),
    })?;
    let runtime_gas = StorageHandle::enter(&mut provider, |storage| {
        storage.gas_used().map_err(|error| error.to_string())
    })?;
    let latency_ns = elapsed_ns(started);
    let captured = capture_execution(
        &provider,
        event_offset,
        GasLedger::SystemInternal,
        runtime_gas,
        "gem_factory",
    )?;

    let mut observation = Observation::new(
        [(GasLedger::SystemInternal, captured.gas_total)],
        captured.gas_components,
    )
    .with_total_latency(latency_ns);
    match prepared.path {
        PreparedGemPath::Direct => {
            StorageHandle::enter(&mut provider, |storage| {
                outbe_gem::api::get_item(&storage, output).map_err(|error| error.to_string())
            })?
            .ok_or_else(|| "created Gem is not canonically readable".to_owned())?;
            observation = observation
                .with_latency("chain.gem.mint", latency_ns)
                .with_postcondition("gem.created", "true");
        }
        PreparedGemPath::Position => {
            let record = StorageHandle::enter(&mut provider, |storage| {
                GemFactoryContract::new(storage)
                    .positions
                    .get(output)
                    .map_err(|error| error.to_string())
            })?
            .ok_or_else(|| "created GemPosition is not canonically readable".to_owned())?;
            observation = observation
                .with_latency("chain.gem_position.mint_marginal", latency_ns)
                .with_postcondition("gem_position.created", "true")
                .with_postcondition(
                    "gem_position.owner_matches",
                    (record.merchant == ALICE).to_string(),
                );
        }
        PreparedGemPath::Merchant {
            position_id,
            initial_capacity,
        } => {
            let (gem, remaining) = StorageHandle::enter(&mut provider, |storage| {
                let gem = outbe_gem::api::get_item(&storage, output)
                    .map_err(|error| error.to_string())?;
                let remaining = GemFactoryContract::new(storage)
                    .positions
                    .get(position_id)
                    .map_err(|error| error.to_string())?
                    .map(|position| position.remaining_capacity_minor);
                Ok::<_, String>((gem, remaining))
            })?;
            gem.ok_or_else(|| "created Merchant Gem is not readable".to_owned())?;
            let remaining = remaining.ok_or_else(|| "source GemPosition disappeared".to_owned())?;
            let capacity_drained =
                remaining == initial_capacity.saturating_sub(U256::from(10) * six_decimal_unit());
            if !capacity_drained {
                return Err(
                    "Merchant Gem postcondition does not match the source position".to_owned(),
                );
            }
            observation = observation
                .with_latency("chain.gem.mint_merchant", latency_ns)
                .with_postcondition("gem.created", "true")
                .with_postcondition("gem_position.capacity_drained", "true");
        }
    }
    observation.storage = captured.storage;
    observation.events = captured.events;
    Ok(observation)
}
