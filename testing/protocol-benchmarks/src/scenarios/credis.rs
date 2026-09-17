use std::time::Instant;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_credis::CredisContract;
use outbe_credisfactory::precompile::ICredisFactory;
use outbe_fidelity::enclave_client::test_enclave as fidelity_enclave;
use outbe_gratis::enclave_client::test_enclave as gratis_enclave;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    addresses::{CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS},
    storage::{gas::PRECOMPILE_BASE_GAS, hashmap::HashMapStorageProvider, Bytecode, StorageHandle},
    units::{checked_protocol_to_native, SCALE_1E6_U256},
};
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{derive_modify_key, derive_view_key, modify_mac};

use outbe_tee::pledgenote::*;

use super::support::{capture_execution, elapsed_ns};
use crate::{
    BenchmarkScenario, CalldataStats, CryptoMode, ExecutionClass, GasComponent, GasLedger,
    Observation, Profile, ScenarioMetadata,
};

const CHAIN_ID: u64 = 1;
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const CREATED_AT: u64 = 1_700_000_000;
const BLOCK_NUMBER: u64 = 42;
const ISSUANCE_ISO: u16 = 840;
/// Threshold anchor elected by the benchmarked `requestCredis`.
const REFERENCE_ISO: u16 = ISSUANCE_ISO;
const ALICE: Address = Address::repeat_byte(0xaa);
const CCA: Address = Address::repeat_byte(0xcc);
const ASSET: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x88,
]);

pub struct CredisScenario;

impl CredisScenario {
    #[must_use]
    pub const fn request() -> Self {
        Self
    }
}

pub struct PreparedCredis {
    provider: HashMapStorageProvider,
    spend_auth: Vec<u8>,
}

fn pledge_stables() -> U256 {
    U256::from(2_000_000)
}

fn pledge_cost() -> U256 {
    SCALE_1E6_U256
}

fn native_stake() -> U256 {
    checked_protocol_to_native(pledge_cost()).expect("benchmark stake fits native COEN")
}

fn oracle_rate() -> U256 {
    U256::from(2) * SCALE_1E6_U256
}

fn chain_identity() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}

fn auth(op: GratisOp, amount: U256, nonce: u64) -> ModifyAuth {
    let modify_key = derive_modify_key(&gratis_enclave::state_key(), ALICE)
        .expect("benchmark Gratis modify key derives");
    ModifyAuth {
        mac: modify_mac(&modify_key, ALICE, op, amount, nonce, chain_identity()),
        op_nonce: nonce,
    }
}

fn iso_word(iso: u16) -> Bytes {
    let mut bytes = vec![0_u8; 32];
    bytes[30..].copy_from_slice(&iso.to_be_bytes());
    Bytes::from(bytes)
}

fn seed_world(storage: StorageHandle<'_>) -> Result<Vec<u8>, String> {
    storage
        .increase_balance(
            outbe_primitives::addresses::CCA_REGISTRY_ADDRESS,
            outbe_ccaregistry::constants::BOND_REQUIREMENT,
        )
        .map_err(|error| error.to_string())?;
    outbe_ccaregistry::runtime::bond(
        storage.clone(),
        CCA,
        outbe_ccaregistry::constants::BOND_REQUIREMENT,
        "Test CCA".into(),
    )
    .map_err(|error| error.to_string())?;
    outbe_gratis::api::mint(
        storage.clone(),
        ALICE,
        pledge_cost(),
        auth(GratisOp::Mint, pledge_cost(), 0),
    )
    .map_err(|error| error.to_string())?;
    outbe_fidelity::api::cohort_in(
        storage.clone(),
        ALICE,
        U256::from(100),
        CREATED_AT - 365 * 86_400,
    )
    .map_err(|error| error.to_string())?;
    outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
        .map_err(|error| error.to_string())?;
    outbe_oracle::api::set_exchange_rate(
        storage.clone(),
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        oracle_rate(),
        1,
        CREATED_AT,
    )
    .map_err(|error| error.to_string())?;
    OracleContract::new(storage.clone())
        .policy_rate
        .write(&ISSUANCE_ISO, U256::from(43_000))
        .map_err(|error| error.to_string())?;
    // The elected threshold anchor must be a registered reference currency. This
    // scenario anchors to the issuance currency, whose COEN pair is already seeded
    // above, so the measured path stays one origination without extra oracle setup.
    OracleContract::new(storage.clone())
        .reference_currencies
        .push(REFERENCE_ISO)
        .map_err(|error| error.to_string())?;
    storage
        .set_code(ALICE, Bytecode::new_raw(Bytes::from_static(&[0xef])))
        .map_err(|error| error.to_string())?;

    let quote = Quote {
        asset: ASSET,
        principal_minor: pledge_stables(),
        max_gratis_minor: U256::MAX,
        reference_currency: REFERENCE_ISO,
    };
    let envelope =
        gratis_enclave::owner_envelope(&storage, ALICE, 1, OwnerAction::Create(quote.clone()));
    let encrypted = outbe_gratisfactory::runtime::create_pledge_note(
        storage.clone(),
        &encode(&CreateRequest { quote, envelope })?,
    )
    .map_err(|error| error.to_string())?;
    let view =
        derive_view_key(&gratis_enclave::state_key(), ALICE).map_err(|error| error.to_string())?;
    let receipt = decrypt_receipt(&view, &encrypted)?;
    storage
        .increase_balance(CREDIS_FACTORY_ADDRESS, native_stake())
        .map_err(|error| error.to_string())?;
    encrypt_request(
        outbe_tee_enclave::crypto::x25519_public(&outbe_tee_enclave::dev::PLEDGE_OFFER_SECRET),
        &PrivateRequest::Use {
            chain_id: chain_identity(),
            note_id: receipt.note_id,
            owner_sa: ALICE,
            authorization: use_mac(receipt.secret, chain_identity(), receipt.note_id, ALICE)?,
        },
    )
}

impl BenchmarkScenario for CredisScenario {
    type Prepared = PreparedCredis;

    fn metadata(&self) -> ScenarioMetadata {
        ScenarioMetadata::new(
            "credis/create/request/marginal",
            "Credis request (real confidential state, marginal child calls)",
            ExecutionClass::UserTransaction,
            Profile::Single,
        )
        .with_crypto_mode(CryptoMode::PortableInProcess)
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        gratis_enclave::install();
        fidelity_enclave::install();
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_timestamp(U256::from(CREATED_AT));
        provider.set_block_number(BLOCK_NUMBER);
        provider.enable_sub_call_stub();
        provider.stub_sub_call_at(VAULT_ROUTER_ADDRESS, Bytes::from(vec![0_u8; 32]));
        provider.stub_sub_call_at(ASSET, iso_word(ISSUANCE_ISO));
        let spend_auth = StorageHandle::enter(&mut provider, seed_world)?;
        Ok(PreparedCredis {
            provider,
            spend_auth,
        })
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        let mut provider = prepared.provider.clone();
        provider.set_gas_limit(BLOCK_GAS_LIMIT);
        provider.enable_production_storage_gas_metering();
        provider.enable_storage_trace();
        let event_offset = provider.get_ordered_events().len();
        let calldata = ICredisFactory::issueCredisCall {
            ownerSA: ALICE,
            encryptedUseAuth: prepared.spend_auth.clone().into(),
        }
        .abi_encode();

        let started = Instant::now();
        let output = StorageHandle::enter(&mut provider, |storage| {
            outbe_credisfactory::precompile::dispatch(storage, &calldata, CCA, native_stake())
                .map_err(|error| error.to_string())
        })?;
        let latency_ns = elapsed_ns(started);
        let decoded = ICredisFactory::issueCredisCall::abi_decode_returns(&output)
            .map_err(|error| error.to_string())?;
        let runtime_gas = StorageHandle::enter(&mut provider, |storage| {
            storage.gas_used().map_err(|error| error.to_string())
        })?;
        let captured = capture_execution(
            &provider,
            event_offset,
            GasLedger::UserTransaction,
            runtime_gas,
            "credis_factory",
        )?;

        let (position, pledged) = StorageHandle::enter(&mut provider, |storage| {
            let position = CredisContract::new(storage.clone())
                .get_position(decoded.credisId)
                .map_err(|error| error.to_string())?;
            let pledged = gratis_enclave::query(&storage, ALICE).pledged;
            Ok::<_, String>((position, pledged))
        })?;
        if position.smart_account != ALICE
            || position.collateral_handle == B256::ZERO
            || pledged != pledge_cost()
            || decoded.amountStables != pledge_stables()
        {
            return Err("Credis request postcondition does not match the sealed pledge".to_owned());
        }

        let calldata_stats = CalldataStats::ethereum(&calldata);
        let mut gas_components = vec![
            GasComponent::new(
                GasLedger::UserTransaction,
                "transaction.base",
                calldata_stats.transaction_base_gas,
                1,
            ),
            GasComponent::new(
                GasLedger::UserTransaction,
                "calldata.zero_bytes",
                calldata_stats.zero_byte_gas,
                calldata_stats.zero_bytes,
            ),
            GasComponent::new(
                GasLedger::UserTransaction,
                "calldata.nonzero_bytes",
                calldata_stats.nonzero_byte_gas,
                calldata_stats.nonzero_bytes,
            ),
            GasComponent::new(
                GasLedger::UserTransaction,
                "precompile.configured_base",
                PRECOMPILE_BASE_GAS,
                1,
            ),
        ];
        gas_components.extend(captured.gas_components);
        let total_gas = calldata_stats
            .intrinsic_gas()
            .saturating_add(PRECOMPILE_BASE_GAS)
            .saturating_add(captured.gas_total);
        let mut observation =
            Observation::new([(GasLedger::UserTransaction, total_gas)], gas_components)
                .with_total_latency(latency_ns)
                .with_latency("chain.credis.request_marginal", latency_ns)
                .with_calldata(calldata_stats)
                .with_postcondition("credis.created", "true")
                .with_postcondition("credis.opaque_collateral_handle", "true")
                .with_postcondition("credis.collateral_pledged", "true")
                .with_postcondition("credis.child_frame_gas_included", "false")
                .with_postcondition("credis.position_id", decoded.credisId.to_string());
        observation.storage = captured.storage;
        observation.events = captured.events;
        Ok(observation)
    }
}
