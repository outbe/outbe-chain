use std::time::Instant;

use alloy_primitives::{Address, Bytes, U256};
use outbe_intex::SeriesId;
use outbe_intexfactory::constants::ORIGIN_ROUTER_ADDRESS;
use outbe_intexfactory::{bench_support::IssuanceLeg, IssuanceParams};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::time::WorldwideDay;

use super::support::{capture_execution, elapsed_ns};
use crate::{BenchmarkScenario, ExecutionClass, GasLedger, Observation, Profile, ScenarioMetadata};

const CHAIN_ID: u64 = 1;
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const T_NOW: u64 = 1_700_000_000;
const TARGET_WWD: WorldwideDay = WorldwideDay::new(20_260_730);
const RECIPIENT: Address = Address::repeat_byte(0x31);
const SERIES_CODES: [[u8; 3]; 10] = [
    *b"USD", *b"EUR", *b"GBP", *b"JPY", *b"CAD", *b"AUD", *b"CHF", *b"CNY", *b"SEK", *b"NZD",
];

#[derive(Clone, Copy)]
enum IntexPath {
    Issue,
    Send,
}

pub struct IntexScenario {
    path: IntexPath,
    profile: Profile,
}

impl IntexScenario {
    #[must_use]
    pub const fn issue(profile: Profile) -> Self {
        Self {
            path: IntexPath::Issue,
            profile,
        }
    }

    #[must_use]
    pub const fn send(profile: Profile) -> Self {
        Self {
            path: IntexPath::Send,
            profile,
        }
    }
}

pub struct PreparedIntex {
    provider: HashMapStorageProvider,
    params: Vec<IssuanceParams>,
    legs: Vec<IssuanceLeg>,
}

fn item_count(profile: Profile) -> usize {
    match profile {
        Profile::Single => 1,
        Profile::Typical => 10,
    }
}

fn params(index: usize) -> IssuanceParams {
    let series_id = SeriesId::pack(TARGET_WWD, SERIES_CODES[index], b'U')
        .expect("benchmark Intex id is canonical");
    IssuanceParams {
        series_id,
        worldwide_day: TARGET_WWD,
        issued_intex_count: 100,
        promis_load_minor: 1_000_000,
        entry_price_minor: U256::from(1_000_000),
        issuance_currency: 840,
        reference_currency: 840,
        recipients: vec![RECIPIENT],
        quantities: vec![U256::from(100)],
        recipient_chains: vec![1],
        snapshot_chains: vec![1],
    }
}

fn new_provider() -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(1);
    provider.set_timestamp(U256::from(T_NOW));
    provider.stub_sub_call_at(ORIGIN_ROUTER_ADDRESS, Bytes::from(vec![0_u8; 32]));
    provider
}

impl BenchmarkScenario for IntexScenario {
    type Prepared = PreparedIntex;

    fn metadata(&self) -> ScenarioMetadata {
        let profile_name = match self.profile {
            Profile::Single => "single",
            Profile::Typical => "typical-10",
        };
        let (id, display_name) = match self.path {
            IntexPath::Issue => (
                format!("intex/create/issue/{profile_name}"),
                format!("Intex series issue ({profile_name})"),
            ),
            IntexPath::Send => (
                format!("intex/create/send-marginal/{profile_name}"),
                format!("Intex issuance send, marginal parent ({profile_name})"),
            ),
        };
        ScenarioMetadata::new(
            id,
            display_name,
            ExecutionClass::InternalTransition,
            self.profile,
        )
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        let mut provider = new_provider();
        let params = (0..item_count(self.profile))
            .map(params)
            .collect::<Vec<_>>();
        let legs = if matches!(self.path, IntexPath::Send) {
            StorageHandle::enter(&mut provider, |storage| {
                params.iter().try_fold(Vec::new(), |mut all, params| {
                    all.extend(
                        outbe_intexfactory::api::issue(&storage, params.clone())
                            .map_err(|error| error.to_string())?,
                    );
                    Ok::<_, String>(all)
                })
            })?
        } else {
            Vec::new()
        };
        Ok(PreparedIntex {
            provider,
            params,
            legs,
        })
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        match self.path {
            IntexPath::Issue => measure_issue(prepared),
            IntexPath::Send => measure_send(prepared),
        }
    }
}

fn measured_provider(prepared: &PreparedIntex) -> (HashMapStorageProvider, usize) {
    let mut provider = prepared.provider.clone();
    provider.set_gas_limit(BLOCK_GAS_LIMIT);
    provider.enable_production_storage_gas_metering();
    provider.enable_storage_trace();
    let event_offset = provider.get_ordered_events().len();
    (provider, event_offset)
}

fn measure_issue(prepared: &PreparedIntex) -> Result<Observation, String> {
    let (mut provider, event_offset) = measured_provider(prepared);
    let started = Instant::now();
    let legs = StorageHandle::enter(&mut provider, |storage| {
        prepared
            .params
            .iter()
            .try_fold(Vec::new(), |mut all, params| {
                all.extend(
                    outbe_intexfactory::api::issue(&storage, params.clone())
                        .map_err(|error| error.to_string())?,
                );
                Ok::<_, String>(all)
            })
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
        "intex_factory",
    )?;
    let all_readable = StorageHandle::enter(&mut provider, |storage| {
        prepared.params.iter().try_fold(true, |all, params| {
            outbe_intex::api::get_series(&storage, params.series_id)
                .map(|series| all && series.is_some())
                .map_err(|error| error.to_string())
        })
    })?;
    if !all_readable {
        return Err("at least one issued Intex series is not readable".to_owned());
    }
    let mut observation = Observation::new(
        [(GasLedger::SystemInternal, captured.gas_total)],
        captured.gas_components,
    )
    .with_total_latency(latency_ns)
    .with_latency("chain.intex.issue", latency_ns)
    .with_postcondition("intex.created_count", prepared.params.len().to_string())
    .with_postcondition("intex.issuance_legs", legs.len().to_string())
    .with_postcondition("intex.all_readable", "true");
    observation.storage = captured.storage;
    observation.events = captured.events;
    Ok(observation)
}

fn measure_send(prepared: &PreparedIntex) -> Result<Observation, String> {
    let (mut provider, event_offset) = measured_provider(prepared);
    let started = Instant::now();
    StorageHandle::enter(&mut provider, |storage| {
        outbe_intexfactory::api::send_issuance(&storage, prepared.legs.clone())
            .map_err(|error| error.to_string())
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
        "intex_factory",
    )?;
    let mut observation = Observation::new(
        [(GasLedger::SystemInternal, captured.gas_total)],
        captured.gas_components,
    )
    .with_total_latency(latency_ns)
    .with_latency("chain.intex.send_marginal", latency_ns)
    .with_postcondition("intex.issuance_legs", prepared.legs.len().to_string())
    .with_postcondition("intex.send_completed", "true")
    .with_postcondition("intex.child_frame_gas_included", "false");
    observation.storage = captured.storage;
    observation.events = captured.events;
    Ok(observation)
}
