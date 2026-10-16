use std::time::Instant;

use alloy_primitives::{Address, U256};
use outbe_nod::NodIssueParams;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::time::WorldwideDay;

use super::support::{capture_execution, elapsed_ns};
use crate::{BenchmarkScenario, ExecutionClass, GasLedger, Observation, Profile, ScenarioMetadata};

const CHAIN_ID: u64 = 1;
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const TARGET_WWD: WorldwideDay = WorldwideDay::new(20_260_812);

pub struct NodScenario {
    profile: Profile,
}

impl NodScenario {
    #[must_use]
    pub const fn direct(profile: Profile) -> Self {
        Self { profile }
    }
}

pub struct PreparedNod {
    provider: HashMapStorageProvider,
    encrypted: Vec<outbe_primitives::nod_encryption::EncryptedNodV2>,
}

fn item_count(profile: Profile) -> usize {
    match profile {
        Profile::Single => 1,
        Profile::Typical => 10,
    }
}

fn issue_params(index: usize) -> NodIssueParams {
    let owner_byte = u8::try_from(index + 1).expect("benchmark cardinality fits one byte");
    NodIssueParams {
        owner: Address::repeat_byte(owner_byte),
        gratis_load_minor: U256::from(1_000),
        worldwide_day: TARGET_WWD,
        league_id: 1,
        entry_price_minor: U256::from(500),
        issuance_currency: 840,
        reference_currency: 840,
    }
}

impl BenchmarkScenario for NodScenario {
    type Prepared = PreparedNod;

    fn metadata(&self) -> ScenarioMetadata {
        let profile_name = match self.profile {
            Profile::Single => "single",
            Profile::Typical => "typical-10",
        };
        ScenarioMetadata::new(
            format!("nod/create/direct/{profile_name}"),
            format!("Nod direct creation ({profile_name})"),
            ExecutionClass::InternalTransition,
            self.profile,
        )
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_block_number(1);
        provider.set_timestamp(U256::from(1_700_000_000));
        let count = item_count(self.profile);
        let encrypted = (0..count)
            .map(|index| outbe_nod::test_support::encrypted_fixture(&issue_params(index), CHAIN_ID))
            .collect();
        Ok(PreparedNod {
            provider,
            encrypted,
        })
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        measure_direct(prepared)
    }
}

fn measure_direct(prepared: &PreparedNod) -> Result<Observation, String> {
    let mut provider = prepared.provider.clone();
    provider.set_gas_limit(BLOCK_GAS_LIMIT);
    provider.enable_production_storage_gas_metering();
    provider.enable_storage_trace();
    let event_offset = provider.get_ordered_events().len();

    let started = Instant::now();
    let (ids, runtime_gas) = StorageHandle::enter(&mut provider, |storage| {
        let mut ids = Vec::with_capacity(prepared.encrypted.len());
        for encrypted in &prepared.encrypted {
            ids.push(
                outbe_nodfactory::api::issue_nod(&storage, encrypted)
                    .map_err(|error| error.to_string())?,
            );
        }
        let gas = storage.gas_used().map_err(|error| error.to_string())?;
        Ok::<_, String>((ids, gas))
    })?;
    let latency_ns = elapsed_ns(started);
    let captured = capture_execution(
        &provider,
        event_offset,
        GasLedger::SystemInternal,
        runtime_gas,
        "nod",
    )?;

    let all_readable = StorageHandle::enter(&mut provider, |storage| {
        ids.iter().try_fold(true, |all, id| {
            outbe_nod::api::get_item(&storage, *id)
                .map(|item| all && item.is_some())
                .map_err(|error| error.to_string())
        })
    })?;
    if !all_readable {
        return Err("at least one created Nod is not canonically readable".to_owned());
    }

    let mut observation = Observation::new(
        [(GasLedger::SystemInternal, captured.gas_total)],
        captured.gas_components,
    )
    .with_total_latency(latency_ns)
    .with_latency("chain.nod.issue_direct", latency_ns)
    .with_postcondition("nod.created_count", ids.len().to_string())
    .with_postcondition("nod.all_readable", "true");
    observation.storage = captured.storage;
    observation.events = captured.events;
    Ok(observation)
}
