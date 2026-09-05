use std::time::Instant;

use alloy_primitives::U256;
use outbe_metadosis::WwdMembership;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::time::WorldwideDay;

use super::support::{capture_execution, elapsed_ns};
use crate::{BenchmarkScenario, ExecutionClass, GasLedger, Observation, Profile, ScenarioMetadata};

const CHAIN_ID: u64 = 1;
const BLOCK_NUMBER: u64 = 10;
const TARGET_WWD: WorldwideDay = WorldwideDay::new(20_260_825);
const BLOCK_GAS_LIMIT: u64 = 30_000_000;

pub struct MetadosisScenario;

impl MetadosisScenario {
    #[must_use]
    pub const fn worldwide_day() -> Self {
        Self
    }
}

impl BenchmarkScenario for MetadosisScenario {
    type Prepared = HashMapStorageProvider;

    fn metadata(&self) -> ScenarioMetadata {
        ScenarioMetadata::new(
            "metadosis/create/worldwide-day",
            "Metadosis worldwide-day creation",
            ExecutionClass::InternalTransition,
            Profile::Single,
        )
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_block_number(BLOCK_NUMBER);
        provider.set_timestamp(U256::from(TARGET_WWD.start_timestamp()));
        Ok(provider)
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        let mut provider = prepared.clone();
        provider.set_gas_limit(BLOCK_GAS_LIMIT);
        provider.enable_production_storage_gas_metering();
        provider.enable_storage_trace();
        let event_offset = provider.get_ordered_events().len();
        let started = Instant::now();
        StorageHandle::enter(&mut provider, |storage| {
            outbe_metadosis::bench_support::create_worldwide_day(
                storage,
                BLOCK_NUMBER,
                TARGET_WWD.start_timestamp(),
                CHAIN_ID,
                TARGET_WWD,
            )
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
            "metadosis",
        )?;
        let projection = StorageHandle::enter(&mut provider, |storage| {
            outbe_metadosis::api::worldwide_day(storage, TARGET_WWD)
                .map_err(|error| error.to_string())
        })?
        .ok_or_else(|| "created Metadosis WWD is not readable".to_owned())?;
        if projection.membership != WwdMembership::Active {
            return Err("created Metadosis WWD is not active".to_owned());
        }

        let mut observation = Observation::new(
            [(GasLedger::SystemInternal, captured.gas_total)],
            captured.gas_components,
        )
        .with_total_latency(latency_ns)
        .with_latency("chain.metadosis.create_worldwide_day", latency_ns)
        .with_postcondition("metadosis.wwd_created", "true")
        .with_postcondition("metadosis.wwd_active", "true")
        .with_postcondition("metadosis.wwd", TARGET_WWD.value().to_string());
        observation.storage = captured.storage;
        observation.events = captured.events;
        Ok(observation)
    }
}
