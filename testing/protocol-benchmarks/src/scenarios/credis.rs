use std::time::Instant;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_credis::CredisContract;
use outbe_credisfactory::precompile::ICredisFactory;
use outbe_fidelity::enclave_client::test_enclave as fidelity_enclave;
use outbe_gratis::enclave_client::test_enclave as gratis_enclave;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    addresses::{CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS},
    math::scaled_math::checked_quote,
    storage::{gas::PRECOMPILE_BASE_GAS, hashmap::HashMapStorageProvider, Bytecode, StorageHandle},
    time::{previous_date_key, timestamp_to_date_key},
    units::{checked_protocol_to_native, SCALE_1E18, SCALE_1E6_U256},
};
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{
    decrypt_pledged, derive_modify_key, derive_view_key, modify_mac, pledge_secret, spend_auth_mac,
};
use outbe_vaultrouter::{LiquidityReservation, VaultRouterContract};

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
/// Threshold anchor elected by the benchmarked `issueCredis`.
const REFERENCE_ISO: u16 = ISSUANCE_ISO;
const ALICE: Address = Address::repeat_byte(0xaa);
const CCA: Address = Address::repeat_byte(0xcc);
const ASSET: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x88,
]);

sol! {
    interface IERC20Metadata {
        function decimals() external view returns (uint8);
    }
}

pub struct CredisScenario;

impl CredisScenario {
    #[must_use]
    pub const fn request() -> Self {
        Self
    }
}

pub struct PreparedCredis {
    provider: HashMapStorageProvider,
    pledge_note: B256,
    spend_auth: [u8; 32],
    reservation_id: U256,
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

fn seed_world(storage: StorageHandle<'_>) -> Result<(B256, [u8; 32], U256), String> {
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
    // above. Issuance also requires that pair's previous closed UTC-day VWAP.
    let oracle = OracleContract::new(storage.clone());
    oracle
        .reference_currencies
        .push(REFERENCE_ISO)
        .map_err(|error| error.to_string())?;
    let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), REFERENCE_ISO)
        .map_err(|error| error.to_string())?
        .ok_or("benchmark COEN pair is not registered")?;
    let closed_day = previous_date_key(timestamp_to_date_key(CREATED_AT));
    oracle
        .record_utc_day_vwap(closed_day, index, oracle_rate())
        .map_err(|error| error.to_string())?;
    oracle
        .utc_day_vwap_last_finalized
        .write(closed_day)
        .map_err(|error| error.to_string())?;
    storage
        .set_code(ALICE, Bytecode::new_raw(Bytes::from_static(&[0xef])))
        .map_err(|error| error.to_string())?;

    let contract = VaultRouterContract::new(storage.clone());
    let reservation_id = U256::from(1);
    contract
        .reservations
        .create(&LiquidityReservation {
            id: reservation_id,
            asset: ASSET,
            amount: pledge_stables(),
            smart_account: ALICE,
            cca: CCA,
            vault: Address::repeat_byte(0x77),
            expires_at: CREATED_AT + 15 * 60,
        })
        .map_err(|error| error.to_string())?;
    // NB: Seal a known quote for the issuance benchmark while the Oracle's
    // previous eight-hour VWAP helper is unimplemented, as in the Credis tests.
    let valuation_price = U256::from(2) * SCALE_1E18;
    let (gratis_cost, entry_price) =
        checked_quote(pledge_stables(), 6, valuation_price).map_err(|error| error.to_string())?;
    let pledge_note = outbe_gratis::api::pledge(
        storage.clone(),
        ALICE,
        pledge_stables(),
        outbe_gratis::api::PledgeTerms {
            stables_amount: pledge_stables(),
            gratis_amount: gratis_cost,
            asset: ASSET,
            entry_price,
            issuance_currency: ISSUANCE_ISO,
            asset_decimals: 6,
            valuation_price,
        },
        auth(GratisOp::Pledge, pledge_stables(), 1),
    )
    .map_err(|error| error.to_string())?;
    if gratis_cost != pledge_cost() {
        return Err("Credis benchmark pledge price drifted".to_owned());
    }
    storage
        .increase_balance(CREDIS_FACTORY_ADDRESS, native_stake())
        .map_err(|error| error.to_string())?;
    let modify_key = derive_modify_key(&gratis_enclave::state_key(), ALICE)
        .map_err(|error| error.to_string())?;
    let spend_auth = spend_auth_mac(&pledge_secret(&modify_key, pledge_note), ALICE);
    Ok((pledge_note, spend_auth, reservation_id))
}

impl BenchmarkScenario for CredisScenario {
    type Prepared = PreparedCredis;

    fn metadata(&self) -> ScenarioMetadata {
        ScenarioMetadata::new(
            "credis/create/request/marginal",
            "Credis issue (real confidential state, marginal child calls)",
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
        provider.stub_sub_call_at_selector(
            ASSET,
            IERC20Metadata::decimalsCall::SELECTOR,
            iso_word(6),
        );
        let (pledge_note, spend_auth, reservation_id) =
            StorageHandle::enter(&mut provider, seed_world)?;
        Ok(PreparedCredis {
            provider,
            pledge_note,
            spend_auth,
            reservation_id,
        })
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        let mut provider = prepared.provider.clone();
        provider.set_gas_limit(BLOCK_GAS_LIMIT);
        provider.enable_production_storage_gas_metering();
        provider.enable_storage_trace();
        let event_offset = provider.get_ordered_events().len();
        let calldata = ICredisFactory::issueCredisCall {
            smartAccount: ALICE,
            pledgeNote: prepared.pledge_note,
            spendAuth: B256::from(prepared.spend_auth),
            referenceCurrency: REFERENCE_ISO,
            reservationId: prepared.reservation_id,
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

        let (position, revealed_owner, pledged) = StorageHandle::enter(&mut provider, |storage| {
            let position = CredisContract::new(storage.clone())
                .get_position(decoded.positionId)
                .map_err(|error| error.to_string())?;
            let owner = outbe_gratis::api::reveal_owner(storage.clone(), &position.eoa_ct)
                .map_err(|error| error.to_string())?;
            let view_key = derive_view_key(&gratis_enclave::state_key(), ALICE)
                .map_err(|error| error.to_string())?;
            let pledged_blob =
                outbe_gratis::api::pledged_ct(storage, ALICE).map_err(|error| error.to_string())?;
            let pledged = decrypt_pledged(&view_key, ALICE, &pledged_blob)
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((position, owner, pledged))
        })?;
        if position.smart_account != ALICE
            || revealed_owner != ALICE
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
                .with_postcondition("credis.owner_revealed", "true")
                .with_postcondition("credis.collateral_pledged", "true")
                .with_postcondition("credis.child_frame_gas_included", "false")
                .with_postcondition("credis.position_id", decoded.positionId.to_string());
        observation.storage = captured.storage;
        observation.events = captured.events;
        Ok(observation)
    }
}
