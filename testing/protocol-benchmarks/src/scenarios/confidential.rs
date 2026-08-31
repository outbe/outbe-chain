use std::time::Instant;

use alloy_primitives::{Address, B256, U256};
use outbe_fidelity::enclave_client::test_enclave as fidelity_enclave;
use outbe_gratis::enclave_client::test_enclave as gratis_enclave;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_promis::enclave_client::test_enclave as promis_enclave;
use outbe_tee::protocol::{FidelityCohortOp, GratisOp, ModifyAuth, PromisOp};

use super::support::{capture_execution, elapsed_ns};
use crate::{
    BenchmarkScenario, CryptoMode, ExecutionClass, GasLedger, Observation, Profile,
    ScenarioMetadata,
};

const CHAIN_ID: u64 = 1;
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const T_NOW: u64 = 1_700_000_000;
const ACCOUNT: Address = Address::repeat_byte(0x64);
const AMOUNT: u64 = 10_000_000;

#[derive(Clone, Copy)]
enum ConfidentialPath {
    Promis,
    Gratis,
    GratisWithFidelity,
    Gratisfactory,
}

pub struct ConfidentialScenario {
    path: ConfidentialPath,
}

impl ConfidentialScenario {
    #[must_use]
    pub const fn promis() -> Self {
        Self {
            path: ConfidentialPath::Promis,
        }
    }

    #[must_use]
    pub const fn gratis() -> Self {
        Self {
            path: ConfidentialPath::Gratis,
        }
    }

    #[must_use]
    pub const fn gratis_with_fidelity() -> Self {
        Self {
            path: ConfidentialPath::GratisWithFidelity,
        }
    }

    #[must_use]
    pub const fn gratisfactory() -> Self {
        Self {
            path: ConfidentialPath::Gratisfactory,
        }
    }
}

pub struct PreparedConfidential {
    provider: HashMapStorageProvider,
}

fn chain_identity() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}

fn gratis_auth() -> ModifyAuth {
    let key = outbe_tee_enclave::gratis::derive_modify_key(&gratis_enclave::state_key(), ACCOUNT)
        .expect("benchmark Gratis key derives");
    ModifyAuth {
        mac: outbe_tee_enclave::gratis::modify_mac(
            &key,
            ACCOUNT,
            GratisOp::Mint,
            U256::from(AMOUNT),
            0,
            chain_identity(),
        ),
        op_nonce: 0,
    }
}

fn promis_auth() -> ModifyAuth {
    let key = outbe_tee_enclave::promis::derive_modify_key(&promis_enclave::state_key(), ACCOUNT)
        .expect("benchmark Promis key derives");
    ModifyAuth {
        mac: outbe_tee_enclave::promis::modify_mac(
            &key,
            ACCOUNT,
            PromisOp::Mint,
            U256::from(AMOUNT),
            0,
            chain_identity(),
        ),
        op_nonce: 0,
    }
}

impl BenchmarkScenario for ConfidentialScenario {
    type Prepared = PreparedConfidential;

    fn metadata(&self) -> ScenarioMetadata {
        let (id, display_name) = match self.path {
            ConfidentialPath::Promis => ("promis/create/mint", "Promis confidential mint"),
            ConfidentialPath::Gratis => ("gratis/create/mint", "Gratis confidential mint"),
            ConfidentialPath::GratisWithFidelity => (
                "gratis/create/mint-with-fidelity",
                "Gratis mint with fused Fidelity round-trip",
            ),
            ConfidentialPath::Gratisfactory => (
                "gratisfactory/create/mint",
                "Gratisfactory mint orchestration",
            ),
        };
        ScenarioMetadata::new(
            id,
            display_name,
            ExecutionClass::InternalTransition,
            Profile::Single,
        )
        .with_crypto_mode(CryptoMode::PortableInProcess)
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        promis_enclave::install();
        gratis_enclave::install();
        fidelity_enclave::install();
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_block_number(1);
        provider.set_timestamp(U256::from(T_NOW));
        Ok(PreparedConfidential { provider })
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        let mut provider = prepared.provider.clone();
        provider.set_gas_limit(BLOCK_GAS_LIMIT);
        provider.enable_production_storage_gas_metering();
        provider.enable_storage_trace();
        let event_offset = provider.get_ordered_events().len();
        let started = Instant::now();
        StorageHandle::enter(&mut provider, |storage| match self.path {
            ConfidentialPath::Promis => {
                outbe_promisfactory::api::mint(storage, ACCOUNT, U256::from(AMOUNT), promis_auth())
                    .map_err(|error| error.to_string())
            }
            ConfidentialPath::Gratis => {
                outbe_gratis::api::mint(storage, ACCOUNT, U256::from(AMOUNT), gratis_auth())
                    .map_err(|error| error.to_string())
            }
            ConfidentialPath::GratisWithFidelity => {
                let section = outbe_fidelity::api::cohort_section(
                    storage.clone(),
                    ACCOUNT,
                    FidelityCohortOp::In,
                    T_NOW,
                )
                .map_err(|error| error.to_string())?;
                let outcome = outbe_gratis::api::mint_with_fidelity(
                    storage.clone(),
                    ACCOUNT,
                    U256::from(AMOUNT),
                    gratis_auth(),
                    section,
                )
                .map_err(|error| error.to_string())?;
                outbe_fidelity::api::apply_fidelity_outcome(storage, ACCOUNT, &outcome)
                    .map_err(|error| error.to_string())
            }
            ConfidentialPath::Gratisfactory => {
                outbe_gratisfactory::api::mint(storage, ACCOUNT, U256::from(AMOUNT), gratis_auth())
                    .map_err(|error| error.to_string())
            }
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
            "confidential_ledger",
        )?;

        let (balance, fidelity_applied) = StorageHandle::enter(&mut provider, |storage| {
            let fidelity_applied = match self.path {
                ConfidentialPath::GratisWithFidelity | ConfidentialPath::Gratisfactory => {
                    outbe_fidelity::api::league(storage.clone(), ACCOUNT)
                        .map(|league| league > 0)
                        .map_err(|error| error.to_string())?
                }
                _ => false,
            };
            let balance = match self.path {
                ConfidentialPath::Promis => {
                    let key = outbe_tee_enclave::promis::derive_view_key(
                        &promis_enclave::state_key(),
                        ACCOUNT,
                    )
                    .map_err(|error| error.to_string())?;
                    let blob = outbe_promis::api::balance_ct(storage, ACCOUNT)
                        .map_err(|error| error.to_string())?;
                    outbe_tee_enclave::promis::decrypt_balance(&key, ACCOUNT, &blob)
                        .map_err(|error| error.to_string())?
                }
                _ => {
                    let key = outbe_tee_enclave::gratis::derive_view_key(
                        &gratis_enclave::state_key(),
                        ACCOUNT,
                    )
                    .map_err(|error| error.to_string())?;
                    let blob = outbe_gratis::api::balance_ct(storage, ACCOUNT)
                        .map_err(|error| error.to_string())?;
                    outbe_tee_enclave::gratis::decrypt_balance(&key, ACCOUNT, &blob)
                        .map_err(|error| error.to_string())?
                }
            };
            Ok::<_, String>((balance, fidelity_applied))
        })?;
        if balance != U256::from(AMOUNT) {
            return Err("confidential balance does not match minted amount".to_owned());
        }

        let component = match self.path {
            ConfidentialPath::Promis => "chain.promis.mint",
            ConfidentialPath::Gratis => "chain.gratis.mint",
            ConfidentialPath::GratisWithFidelity => "chain.gratis.mint_with_fidelity",
            ConfidentialPath::Gratisfactory => "chain.gratisfactory.mint",
        };
        let mut observation = Observation::new(
            [(GasLedger::SystemInternal, captured.gas_total)],
            captured.gas_components,
        )
        .with_total_latency(latency_ns)
        .with_latency(component, latency_ns)
        .with_postcondition("confidential.balance_matches", "true")
        .with_postcondition(
            "confidential.fidelity_applied",
            fidelity_applied.to_string(),
        );
        observation.storage = captured.storage;
        observation.events = captured.events;
        Ok(observation)
    }
}
