use std::time::Instant;

use alloy_primitives::{Address, U256};
use outbe_primitives::{
    stablecoin::{encode_canonical_stablecoin_create, StablecoinCreatePayload},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use outbe_stablecoinfactory::{
    api::{FactoryReservation, StablecoinFactoryApi, ValidatedStablecoinCreate},
    StablecoinFactoryContract,
};

use super::support::{capture_execution, elapsed_ns};
use crate::{BenchmarkScenario, ExecutionClass, GasLedger, Observation, Profile, ScenarioMetadata};

const CHAIN_ID: u64 = 1;
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const ISSUER: Address = Address::repeat_byte(0x11);
const PROPOSAL_ID: U256 = U256::from_limbs([7, 0, 0, 0]);

#[derive(Clone, Copy)]
enum StablecoinPath {
    Reserve,
    Approved,
}

pub struct StablecoinScenario {
    path: StablecoinPath,
}

impl StablecoinScenario {
    #[must_use]
    pub const fn reserve() -> Self {
        Self {
            path: StablecoinPath::Reserve,
        }
    }

    #[must_use]
    pub const fn approved() -> Self {
        Self {
            path: StablecoinPath::Approved,
        }
    }
}

pub struct PreparedStablecoin {
    provider: HashMapStorageProvider,
    raw_payload: Vec<u8>,
    validated: ValidatedStablecoinCreate,
}

fn payload() -> StablecoinCreatePayload {
    StablecoinCreatePayload {
        issuer: ISSUER,
        name: "Example Dollar".to_owned(),
        ticker: "EXUSD".to_owned(),
        iso4217: 840,
        decimals: 6,
        supply_cap: U256::from(1_000_000_000_000_u64),
        policy_id: U256::from(1),
    }
}

fn reservation(validated: &ValidatedStablecoinCreate) -> FactoryReservation {
    FactoryReservation {
        proposal_id: PROPOSAL_ID,
        token_id: validated.token_id,
        ticker: validated.payload.ticker.clone(),
        token: validated.token,
    }
}

impl BenchmarkScenario for StablecoinScenario {
    type Prepared = PreparedStablecoin;

    fn metadata(&self) -> ScenarioMetadata {
        let (id, name) = match self.path {
            StablecoinPath::Reserve => (
                "stablecoin/create/reserve",
                "Stablecoin governance validation and reservation",
            ),
            StablecoinPath::Approved => (
                "stablecoin/create/execute-approved",
                "Stablecoin approved creation",
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
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_block_number(1);
        let raw_payload =
            encode_canonical_stablecoin_create(&payload()).map_err(|error| error.to_string())?;
        let validated = StorageHandle::enter(&mut provider, |storage| {
            StablecoinFactoryApi::validate_create(storage, ISSUER, &raw_payload)
                .map_err(|error| error.to_string())
        })?;
        if matches!(self.path, StablecoinPath::Approved) {
            StorageHandle::enter(&mut provider, |storage| {
                StablecoinFactoryApi::reserve(storage, &reservation(&validated))
                    .map_err(|error| error.to_string())
            })?;
        }
        Ok(PreparedStablecoin {
            provider,
            raw_payload,
            validated,
        })
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        let mut provider = prepared.provider.clone();
        provider.set_gas_limit(BLOCK_GAS_LIMIT);
        provider.enable_production_storage_gas_metering();
        provider.enable_storage_trace();
        let event_offset = provider.get_ordered_events().len();
        let started = Instant::now();
        let created = StorageHandle::enter(&mut provider, |storage| match self.path {
            StablecoinPath::Reserve => {
                let validated = StablecoinFactoryApi::validate_create(
                    storage.clone(),
                    ISSUER,
                    &prepared.raw_payload,
                )
                .map_err(|error| error.to_string())?;
                StablecoinFactoryApi::reserve(storage, &reservation(&validated))
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>(validated)
            }
            StablecoinPath::Approved => StablecoinFactoryApi::execute_approved(
                storage,
                PROPOSAL_ID,
                ISSUER,
                &prepared.raw_payload,
                0,
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
            "stablecoin_factory",
        )?;

        let mut observation = Observation::new(
            [(GasLedger::SystemInternal, captured.gas_total)],
            captured.gas_components,
        )
        .with_total_latency(latency_ns);
        match self.path {
            StablecoinPath::Reserve => {
                let owner = StorageHandle::enter(&mut provider, |storage| {
                    StablecoinFactoryContract::new(storage)
                        .pending_token_id
                        .read(&created.token_id)
                        .map_err(|error| error.to_string())
                })?;
                if owner != PROPOSAL_ID {
                    return Err("Stablecoin reservation owner does not match proposal".to_owned());
                }
                observation = observation
                    .with_latency("chain.stablecoin.reserve", latency_ns)
                    .with_postcondition("stablecoin.reserved", "true");
            }
            StablecoinPath::Approved => {
                let identity = StorageHandle::enter(&mut provider, |storage| {
                    outbe_stablecoin::api::identity(storage, created.token)
                        .map_err(|error| error.to_string())
                })?;
                if identity.token_id != prepared.validated.token_id
                    || identity.issuer != ISSUER
                    || identity.symbol != prepared.validated.payload.ticker
                {
                    return Err("created Stablecoin identity drifted from admission".to_owned());
                }
                observation = observation
                    .with_latency("chain.stablecoin.execute_approved", latency_ns)
                    .with_postcondition("stablecoin.created", "true")
                    .with_postcondition("stablecoin.identity_matches", "true")
                    .with_postcondition("stablecoin.token", format!("{:#x}", created.token));
            }
        }
        observation.storage = captured.storage;
        observation.events = captured.events;
        Ok(observation)
    }
}
