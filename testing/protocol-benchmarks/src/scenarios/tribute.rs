//! Permanent local gas + latency benchmark for successful Tribute creation.
//!
//! Run from the repository root:
//! `cargo bench -p outbe-protocol-benchmarks --bench protocol_operations -- run --filter tribute`
//!
//! The benchmark never starts a node, network, Docker, SGX, or a TEE sidecar.
//! It executes both the canonical TributeFactory state transition and the
//! canonical enclave offer processor in-process. Issuance is ZK-only, so the
//! single scenario uses the frozen tribute proof fixture, a registered L2, and
//! a valid BLS MinSig signature over the proof's Merkle root.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall};
use ark_bn254::Fr;
use ark_ff::UniformRand;
use commonware_codec::Encode;
use commonware_cryptography::bls12381::primitives::{
    ops::{self, sign_message},
    variant::MinSig,
};
use outbe_compressed_entities::{
    begin_block, EntityRef, ExecutionScope, IdPage, IdPageRequest, ParentBodySource,
    ParentBodySourceError, QueryRef, StoredBody,
};
use outbe_l2_demo::{empty_tree, prove_tribute, TributeDemo};
use outbe_l2_zk_canonical::claims::tribute::{
    alloy::PublicInputs as TributePublicInputs, binding, decode_public_inputs, public_words,
    TributeDraftClaim, PUBLIC_INPUT_COUNT,
};
use outbe_l2_zk_canonical::{combined_len, encode_combined_proof, l2_keys, Claim};
use outbe_l2registry::L2RegistryContract;
use outbe_metadosis::{
    genesis::{FreshDevnetGenesisBuilder, GenesisWorldwideDay},
    WwdDayType, WwdStatus,
};
use outbe_oracle::{
    genesis::{init_from_genesis, OracleGenesisConfig},
    schema::OracleContract,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    address_pair::AddressPair,
    addresses::{
        AGENT_REWARD_ADDRESS, COMPRESSED_ENTITIES_ADDRESS, L2_REGISTRY_ADDRESS, METADOSIS_ADDRESS,
        ORACLE_ADDRESS, TRIBUTE_ADDRESS, TRIBUTE_FACTORY_ADDRESS,
    },
    storage::{
        hashmap::{HashMapStorageProvider, StorageTraceKind, StorageTraceOperation},
        StorageHandle,
    },
    time::date_key_to_utc_timestamp,
};
use outbe_tee::OFFER_HKDF_SALT;
use outbe_tee_enclave::{
    crypto::ecdhe_tribute_offer_decrypt,
    process::{process_tribute_offer_batch, TributeOfferKeyMaterial},
};
use outbe_tribute::TributeContract;
use outbe_tributefactory::bench_support::{execute_offer_with_processor, BenchOfferInput};
use outbe_zk_backend::barretenberg::{init_crs, Barretenberg, RawVerifier};
use outbe_zk_core::codec::field_to_b256;
use outbe_zk_core::hash::derive_owner;
use outbe_zk_core::keys::{keypair, NftSecret, Signer};
use outbe_zk_core::zk::ProofGenerator;
use rand::{rngs::StdRng, SeedableRng};
use revm::context_interface::cfg::gas::{SSTORE_RESET, WARM_STORAGE_READ_COST};
use revm::precompile::bn254::{
    pair::{ISTANBUL_PAIR_BASE, ISTANBUL_PAIR_PER_POINT},
    run_pair,
};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use crate::{
    BenchmarkScenario, CalldataStats, CryptoMode, EventCount, ExecutionClass, GasComponent,
    GasLedger, Observation, Profile, ScenarioMetadata, ScenarioReport, StorageOperationKind,
    StorageTraceEntry,
};

const CHAIN_ID: u64 = outbe_primitives::chain::DEVNET_CHAIN_ID;
/// L2 chain id the bench registers and selects; it is the ABI `uint32` selector
/// and the registry key, so a single type keeps both in sync.
const L2_CHAIN_ID: u32 = 0xdead;
/// Circuit version registered for `L2_CHAIN_ID`; the frozen fixture proof
/// verifies under its key.
const L2_CIRCUIT_VERSION: &str = "1.0.0";
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const TARGET_WWD: WorldwideDay = WorldwideDay::new(20_260_802);
const REWARD_UTC_DAY: u32 = 20_260_825;
const CALLER: Address = Address::repeat_byte(0x77);
const SU_HASH: B256 = B256::with_last_byte(0x22);
const OFFER_PRIVATE_KEY: [u8; 32] = [0x33; 32];
const PAIRING_REFERENCE_GAS: u64 = ISTANBUL_PAIR_BASE + 2 * ISTANBUL_PAIR_PER_POINT;
const VERIFIER_SAFETY_MULTIPLIER: f64 = 1.5;
const GAS_ROUNDING: u64 = 10_000;
// Stable ADR-007 cleanup reserves charged explicitly by the CE execution scope.
// Keeping the decomposition here makes a protocol-gas change fail loudly rather
// than disappearing into a generic runtime remainder.
const CE_FIRST_BODY_TOUCH_CLEANUP_GAS: u64 = 55_000;
const CE_BODY_TOUCHED_LENGTH_CLEANUP_GAS: u64 = 5_000;
const CE_FIRST_INDEX_TOUCH_CLEANUP_GAS: u64 = 25_000;
const CE_INDEX_TOUCHED_LENGTH_CLEANUP_GAS: u64 = 5_000;
const FROZEN_TRIBUTE_PROOF: &[u8] = include_bytes!("../../fixtures/tribute_offer_proof.bin");

mod abi {
    use super::*;

    sol! {
        function offerTribute(
            bytes cipherText,
            bytes nonce,
            uint256 ephemeralPubkey,
            uint32 worldwideDay,
            uint16 tributeCurrency,
            uint16 referenceCurrency,
            bool excludeFromIntexIssuance,
            bytes zkProof,
            uint32 chainId,
            string version,
            bytes zkPublicKey,
            bytes zkMerkleRoot,
            bytes signature
        ) external returns (uint256 tributeId);
    }
}

struct NoParentBodies;

impl ParentBodySource for NoParentBodies {
    fn get(&self, _entity: EntityRef) -> Result<Option<StoredBody>, ParentBodySourceError> {
        Ok(None)
    }

    fn list(
        &self,
        _query: QueryRef,
        _request: IdPageRequest,
    ) -> Result<IdPage, ParentBodySourceError> {
        Ok(IdPage {
            ids: Vec::new(),
            next_after: None,
        })
    }
}

struct Fixture {
    plaintext: Vec<u8>,
    cipher_text: Bytes,
    nonce: Bytes,
    ephemeral_pubkey: U256,
    proof: Bytes,
    public_inputs: TributePublicInputs,
    l2_public_key: Vec<u8>,
    signature: Bytes,
    crs_init_ms: f64,
    proof_generation_ms: f64,
}

pub struct TributeScenario;

pub struct PreparedTribute {
    fixture: Fixture,
    provider: HashMapStorageProvider,
}

#[derive(Clone, Copy)]
struct GateMeasurement {
    wall_ms: f64,
}

fn build_payload() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "creator": format!("{CALLER:?}"),
        "tribute_draft_id": format!("{:#x}", B256::with_last_byte(0x11)),
        "amount_base": "100",
        "amount_micro": "0",
        "su_hashes": [format!("{SU_HASH:#x}")],
        "wallet_addresses": [],
        "sra_addresses": [],
    }))
    .expect("benchmark JSON is serializable")
}

/// The verification key chain 57005 registers for the fixture's circuit
/// version — the same bytes the tribute factory hands barretenberg.
fn fixture_vk() -> &'static [u8] {
    l2_keys(u64::from(L2_CHAIN_ID), Claim::Tribute)
        .iter()
        .find(|key| key.version() == L2_CIRCUIT_VERSION)
        .expect("the benchmark's L2 registers the fixture circuit version")
        .vk_bytes()
}

/// The deterministic claim, signer and binding the frozen fixture was proven
/// from. Shared by `build_fixture` (which asserts the frozen proof's public
/// words still match) and by the regeneration test that writes the file.
fn fixture_statement() -> (TributeDraftClaim, Signer, Fr, StdRng) {
    let mut rng = StdRng::from_seed([9; 32]);
    let (secret, public_key) = keypair(&mut rng);
    let owner_nonce = Fr::rand(&mut rng);
    let owner = derive_owner(&public_key, owner_nonce).unwrap();
    let draft_id = B256::with_last_byte(0x11);
    let claim = TributeDraftClaim {
        id: draft_id,
        owner: field_to_b256(&owner).unwrap(),
        worldwide_day: u64::from(TARGET_WWD.value()),
        currency: 840,
        base: 100,
        micro: 0,
        su_ids: vec![SU_HASH],
    };
    // Six preimage elements: the L2 chain id is folded alongside the host, so
    // this proof is not replayable as another L2's.
    let binding_hash = binding(
        &CALLER.into_array(),
        &draft_id.0,
        CHAIN_ID,
        u64::from(L2_CHAIN_ID),
    )
    .unwrap();
    let signer = Signer::from_secret(NftSecret::new(secret), owner_nonce).unwrap();
    (claim, signer, binding_hash, rng)
}

fn build_fixture() -> Fixture {
    let crs_started = Instant::now();
    init_crs().expect("pinned CRS initializes");
    let crs_init_ms = crs_started.elapsed().as_secs_f64() * 1_000.0;

    let plaintext = build_payload();
    let offer_secret = StaticSecret::from(OFFER_PRIVATE_KEY);
    let offer_public = X25519PublicKey::from(&offer_secret).to_bytes();
    let ephemeral_secret = [0x44; 32];
    let ephemeral_pubkey = X25519PublicKey::from(&StaticSecret::from(ephemeral_secret)).to_bytes();
    let nonce = [0x55; 12];
    let cipher_text = outbe_tee::offer_encrypt::encrypt_tribute_offer_with(
        &offer_public,
        ephemeral_secret,
        nonce,
        &plaintext,
    )
    .expect("deterministic offer encryption succeeds");

    let vk = fixture_vk();
    let (claim, signer, binding_hash, mut proof_rng) = fixture_statement();

    let proof_started = Instant::now();
    let path = empty_tree().unwrap().empty_inclusion_path(0);
    let (witness, public) =
        prove_tribute(&mut proof_rng, &claim, &signer, binding_hash, &path).unwrap();
    let generated_proof =
        ProofGenerator::<TributeDemo>::generate(&Barretenberg::default(), &witness, &public)
            .unwrap();
    let proof_generation_ms = proof_started.elapsed().as_secs_f64() * 1_000.0;

    let generated_combined =
        encode_combined_proof(&public_words(&public), &generated_proof.proof, vk).unwrap();
    assert_eq!(
        generated_combined.len(),
        combined_len(vk, PUBLIC_INPUT_COUNT).unwrap()
    );
    let generated_public_inputs: TributePublicInputs =
        decode_public_inputs(&generated_combined, vk)
            .unwrap()
            .try_into()
            .unwrap();
    let bb = Barretenberg::default();
    assert!(bb.verify_combined(vk, &generated_combined).unwrap());

    assert_eq!(
        FROZEN_TRIBUTE_PROOF.len(),
        combined_len(vk, PUBLIC_INPUT_COUNT).unwrap(),
        "frozen benchmark proof has the wrong size"
    );
    let public_inputs: TributePublicInputs = decode_public_inputs(FROZEN_TRIBUTE_PROOF, vk)
        .unwrap()
        .try_into()
        .unwrap();
    assert!(
        bb.verify_combined(vk, FROZEN_TRIBUTE_PROOF).unwrap(),
        "frozen benchmark proof no longer verifies"
    );
    assert_eq!(
        generated_public_inputs, public_inputs,
        "frozen benchmark proof public inputs drifted from the deterministic witness"
    );

    let mut bls_rng =
        <rand_commonware::rngs::StdRng as rand_commonware::SeedableRng>::from_seed([0x5a; 32]);
    let (l2_private_key, l2_public_key) = ops::keypair::<_, MinSig>(&mut bls_rng);
    let signature = sign_message::<MinSig>(
        &l2_private_key,
        outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE,
        public_inputs.merkle_root.as_slice(),
    )
    .encode()
    .to_vec();

    Fixture {
        plaintext,
        cipher_text: cipher_text.into(),
        nonce: nonce.to_vec().into(),
        ephemeral_pubkey: U256::from_be_bytes(ephemeral_pubkey),
        proof: Bytes::copy_from_slice(FROZEN_TRIBUTE_PROOF),
        public_inputs,
        l2_public_key: l2_public_key.encode().to_vec(),
        signature: signature.into(),
        crs_init_ms,
        proof_generation_ms,
    }
}

fn bench_input(fixture: &Fixture) -> BenchOfferInput {
    BenchOfferInput {
        caller: CALLER,
        cipher_text: fixture.cipher_text.clone(),
        nonce: fixture.nonce.clone(),
        ephemeral_pubkey: fixture.ephemeral_pubkey,
        worldwide_day: TARGET_WWD,
        tribute_currency: 840,
        reference_currency: 840,
        exclude_from_intex_issuance: false,
        zk_proof: fixture.proof.clone(),
        l2_chain_id: L2_CHAIN_ID,
        circuit_version: L2_CIRCUIT_VERSION.to_owned(),
        zk_merkle_root: Bytes::copy_from_slice(fixture.public_inputs.merkle_root.as_slice()),
        signature: fixture.signature.clone(),
    }
}

fn calldata(fixture: &Fixture) -> Vec<u8> {
    abi::offerTributeCall {
        cipherText: fixture.cipher_text.clone(),
        nonce: fixture.nonce.clone(),
        ephemeralPubkey: fixture.ephemeral_pubkey,
        worldwideDay: TARGET_WWD.value(),
        tributeCurrency: 840,
        referenceCurrency: 840,
        excludeFromIntexIssuance: false,
        zkProof: fixture.proof.clone(),
        chainId: L2_CHAIN_ID,
        version: L2_CIRCUIT_VERSION.to_owned(),
        zkPublicKey: Bytes::new(),
        zkMerkleRoot: Bytes::copy_from_slice(fixture.public_inputs.merkle_root.as_slice()),
        signature: fixture.signature.clone(),
    }
    .abi_encode()
}

fn seed_offer_world(storage: StorageHandle<'_>) {
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
        .unwrap();
    storage
        .sstore(
            COMPRESSED_ENTITIES_ADDRESS,
            U256::from(1),
            U256::from_be_slice(
                outbe_compressed_entities::sealed_root(B256::ZERO)
                    .unwrap()
                    .as_slice(),
            ),
        )
        .unwrap();

    FreshDevnetGenesisBuilder::new()
        .seed_active_worldwide_day(GenesisWorldwideDay {
            worldwide_day: TARGET_WWD,
            status: WwdStatus::Offering,
            day_type: WwdDayType::Green,
            forming_start: 1,
            forming_end: 2,
            lookback_end: 3,
            offering_end: 4,
            scheduled_process_time: 5,
            metadosis_limit_amount: U256::from(100),
            previous_vwap: U256::from(90),
            current_vwap: U256::from(100),
        })
        .apply(storage.clone())
        .unwrap();

    let mut oracle = OracleContract::new(storage.clone());
    init_from_genesis(&mut oracle, &OracleGenesisConfig::default_config()).unwrap();
    let start = TARGET_WWD.start_timestamp();
    let pair = AddressPair::new_coen_to(840);
    oracle
        .write_snapshot(start + 1, &[(pair, U256::from(100), U256::ONE)])
        .unwrap();
    oracle
        .store_worldwide_day_vwap_snapshot(TARGET_WWD, start, start + 50 * 60 * 60)
        .unwrap();
    TributeContract::new(storage)
        .unseal_day(TARGET_WWD)
        .unwrap();
}

fn seeded_world(fixture: &Fixture) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(
        date_key_to_utc_timestamp(REWARD_UTC_DAY) + 43_200,
    ));
    StorageHandle::enter(&mut provider, |storage| {
        seed_offer_world(storage.clone());
        // Bind the operator's proof root to its registered network key.
        L2RegistryContract::new(storage)
            .register_network(u64::from(L2_CHAIN_ID), CALLER, &fixture.l2_public_key)
            .unwrap();
    });
    provider
}

fn prepared_world(fixture: &Fixture) -> (HashMapStorageProvider, ExecutionScope) {
    let mut provider = seeded_world(fixture);
    let scope = StorageHandle::enter(&mut provider, |storage| {
        let scope = ExecutionScope::new();
        begin_block(storage, &scope).unwrap();
        scope
    });
    provider.set_gas_limit(BLOCK_GAS_LIMIT);
    provider.enable_production_storage_gas_metering();
    (provider, scope)
}

fn measure_gate(fixture: &Fixture) -> GateMeasurement {
    let (mut provider, _scope) = prepared_world(fixture);
    let root = fixture.public_inputs.merkle_root.as_slice();
    let signature = fixture.signature.as_ref();
    let started = Instant::now();
    let gas_used = StorageHandle::enter(&mut provider, |storage| {
        let outcome = outbe_l2registry::api::check_zk_merkle_root_signature(
            storage.clone(),
            u64::from(L2_CHAIN_ID),
            root,
            signature,
        )
        .expect("L2 gate succeeds");
        if !matches!(
            outcome,
            outbe_l2registry::api::ZkOfferCheck::Verified { chain_id } if chain_id == u64::from(L2_CHAIN_ID)
        ) {
            panic!("unexpected L2 gate result: {outcome:?}");
        }
        storage.gas_used().unwrap()
    });
    let wall_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let (reads, writes) = provider.metered_storage_operations();
    let gas = reads * WARM_STORAGE_READ_COST + writes * SSTORE_RESET;
    assert_eq!(gas_used, gas);
    GateMeasurement { wall_ms }
}

fn pairing_input() -> Vec<u8> {
    hex::decode(concat!(
        "1c76476f4def4bb94541d57ebba1193381ffa7aa76ada664dd31c16024c43f59",
        "3034dd2920f673e204fee2811c678745fc819b55d3e9d294e45c9b03a76aef41",
        "209dd15ebff5d46c4bd888e51a93cf99a7329636c63514396b4a452003a35bf7",
        "04bf11ca01483bfa8b34b43561848d28905960114c8ac04049af4b6315a41678",
        "2bb8324af6cfc93537a2ad1a445cfd0ca2a71acd7ac41fadbf933c2a51be344d",
        "120a2a4cf30c1bf9845f20c6fe39e07ea2cce61f0c9bb048165fe5e4de877550",
        "111e129f1cf1097710d41c4ac70fcdfa5ba2023c6ff1cbeac322de49d1b6df7c",
        "2032c61a830e3c17286de9462bf242fca2883585b93870a73853face6a6bf411",
        "198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2",
        "1800deef121f1e76426a00665e5c4479674322d4f75edadd46debd5cd992f6ed",
        "090689d0585ff075ec9e99ad690c3395bc4b313370b38ef355acdadcd122975b",
        "12c85ea5db8c6deb4aab71808dcb408fe3d1e7690c43d37b4ce6cc0166fa7daa",
    ))
    .unwrap()
}

fn measure_pairing_ms(input: &[u8]) -> f64 {
    const INNER_ITERATIONS: u32 = 10;
    let started = Instant::now();
    for _ in 0..INNER_ITERATIONS {
        black_box(
            run_pair(
                black_box(input),
                ISTANBUL_PAIR_PER_POINT,
                ISTANBUL_PAIR_BASE,
                PAIRING_REFERENCE_GAS,
            )
            .expect("reference pairing succeeds"),
        );
    }
    started.elapsed().as_secs_f64() * 1_000.0 / f64::from(INNER_ITERATIONS)
}

fn measure_verify_ms(proof: &[u8]) -> f64 {
    let vk = fixture_vk();
    let started = Instant::now();
    decode_public_inputs(black_box(proof), vk).unwrap();
    assert!(Barretenberg::default()
        .verify_combined(vk, black_box(proof))
        .unwrap());
    started.elapsed().as_secs_f64() * 1_000.0
}

fn measure_encrypt_ms(plaintext: &[u8]) -> f64 {
    let offer_public = X25519PublicKey::from(&StaticSecret::from(OFFER_PRIVATE_KEY)).to_bytes();
    let started = Instant::now();
    black_box(
        outbe_tee::offer_encrypt::encrypt_tribute_offer_with(
            &offer_public,
            [0x44; 32],
            [0x55; 12],
            plaintext,
        )
        .unwrap(),
    );
    started.elapsed().as_secs_f64() * 1_000.0
}

fn measure_enclave_decrypt_ms(fixture: &Fixture) -> f64 {
    const INNER_ITERATIONS: u32 = 10;
    let ephemeral_pubkey = fixture.ephemeral_pubkey.to_be_bytes::<32>();
    let started = Instant::now();
    for _ in 0..INNER_ITERATIONS {
        black_box(
            ecdhe_tribute_offer_decrypt(
                &OFFER_PRIVATE_KEY,
                &OFFER_HKDF_SALT,
                black_box(&ephemeral_pubkey),
                black_box(&fixture.nonce),
                black_box(&fixture.cipher_text),
            )
            .expect("benchmark ciphertext decrypts in the enclave"),
        );
    }
    started.elapsed().as_secs_f64() * 1_000.0 / f64::from(INNER_ITERATIONS)
}

fn measure_abi_ms(fixture: &Fixture) -> f64 {
    const INNER_ITERATIONS: u32 = 100;
    let started = Instant::now();
    for _ in 0..INNER_ITERATIONS {
        black_box(calldata(fixture));
    }
    started.elapsed().as_secs_f64() * 1_000.0 / f64::from(INNER_ITERATIONS)
}

fn ceil_to(value: u64, multiple: u64) -> u64 {
    value.div_ceil(multiple) * multiple
}

impl BenchmarkScenario for TributeScenario {
    type Prepared = PreparedTribute;

    fn metadata(&self) -> ScenarioMetadata {
        ScenarioMetadata::new(
            "tribute/create/zk/cold",
            "Tribute creation (ZK, cold)",
            ExecutionClass::UserTransaction,
            Profile::Single,
        )
        .with_crypto_mode(CryptoMode::PortableInProcess)
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        let fixture = build_fixture();
        let provider = seeded_world(&fixture);
        Ok(PreparedTribute { fixture, provider })
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        measure_scenario_once(prepared)
    }
}

fn measure_scenario_once(prepared: &PreparedTribute) -> Result<Observation, String> {
    let fixture = &prepared.fixture;
    let mut provider = prepared.provider.clone();
    let scope = StorageHandle::enter(&mut provider, |storage| {
        let scope = ExecutionScope::new();
        begin_block(storage, &scope).map_err(|error| error.to_string())?;
        Ok::<_, String>(scope)
    })?;
    provider.set_gas_limit(BLOCK_GAS_LIMIT);
    provider.enable_production_storage_gas_metering();
    provider.enable_storage_trace();
    let event_offset = provider.get_ordered_events().len();

    let input = bench_input(fixture);
    let key = TributeOfferKeyMaterial {
        tribute_offer_private_key: &OFFER_PRIVATE_KEY,
        salt: &OFFER_HKDF_SALT,
    };
    let mut enclave_ns = 0_u64;
    let started = Instant::now();
    let (tribute_id, runtime_gas) = StorageHandle::enter(&mut provider, |storage| {
        let tribute_id = execute_offer_with_processor(
            storage.clone(),
            &scope,
            &NoParentBodies,
            input,
            |offers| {
                let enclave_started = Instant::now();
                let (results, inputs_hash) = process_tribute_offer_batch(&key, offers);
                enclave_ns = elapsed_ns(enclave_started);
                black_box(inputs_hash);
                Ok(results)
            },
        )
        .map_err(|error| error.to_string())?;
        let gas = storage.gas_used().map_err(|error| error.to_string())?;
        Ok::<_, String>((tribute_id, gas))
    })?;
    let full_path_ns = elapsed_ns(started);

    let (reads, writes) = provider.metered_storage_operations();
    let trace = provider.storage_trace().to_vec();
    let trace_reads = u64::try_from(
        trace
            .iter()
            .filter(|operation| operation.kind == StorageTraceKind::Read)
            .count(),
    )
    .unwrap_or(u64::MAX);
    let trace_writes = u64::try_from(
        trace
            .iter()
            .filter(|operation| operation.kind == StorageTraceKind::Write)
            .count(),
    )
    .unwrap_or(u64::MAX);
    if (reads, writes) != (trace_reads, trace_writes) {
        return Err(format!(
            "storage trace differs from production meter: meter=({reads},{writes}), trace=({trace_reads},{trace_writes})"
        ));
    }
    let ordered_events = provider.get_ordered_events()[event_offset..].to_vec();

    let stored = StorageHandle::enter(&mut provider, |storage| {
        TributeContract::new(storage)
            .get_tribute(&scope, &NoParentBodies, tribute_id)
            .map_err(|error| error.to_string())
    })?
    .ok_or_else(|| "created Tribute is not readable through the canonical contract".to_owned())?;
    if stored.owner != CALLER || stored.worldwide_day != TARGET_WWD {
        return Err("created Tribute postcondition has the wrong owner or day".to_owned());
    }

    let calldata = calldata(fixture);
    let calldata_stats = CalldataStats::ethereum(&calldata);
    let configured_base = outbe_tributefactory::precompile::base_gas(&calldata);
    let storage_gas = reads
        .saturating_mul(WARM_STORAGE_READ_COST)
        .saturating_add(writes.saturating_mul(SSTORE_RESET));
    let explicit_runtime_gas = runtime_gas
        .checked_sub(storage_gas)
        .ok_or_else(|| "runtime gas is lower than metered storage gas".to_owned())?;
    let expected_explicit = CE_FIRST_BODY_TOUCH_CLEANUP_GAS
        + CE_BODY_TOUCHED_LENGTH_CLEANUP_GAS
        + 2 * CE_FIRST_INDEX_TOUCH_CLEANUP_GAS
        + CE_INDEX_TOUCHED_LENGTH_CLEANUP_GAS;
    if explicit_runtime_gas != expected_explicit {
        return Err(format!(
            "unexpected explicit runtime gas: expected {expected_explicit}, got {explicit_runtime_gas}"
        ));
    }

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
            configured_base,
            1,
        ),
    ];
    gas_components.extend(storage_gas_components(&trace));
    gas_components.extend([
        GasComponent::new(
            GasLedger::UserTransaction,
            "compressed_entities.cleanup.first_body_touch",
            CE_FIRST_BODY_TOUCH_CLEANUP_GAS,
            1,
        )
        .attributed_to("compressed_entities"),
        GasComponent::new(
            GasLedger::UserTransaction,
            "compressed_entities.cleanup.body_touched_length",
            CE_BODY_TOUCHED_LENGTH_CLEANUP_GAS,
            1,
        )
        .attributed_to("compressed_entities"),
        GasComponent::new(
            GasLedger::UserTransaction,
            "compressed_entities.cleanup.first_index_touch",
            2 * CE_FIRST_INDEX_TOUCH_CLEANUP_GAS,
            2,
        )
        .attributed_to("compressed_entities"),
        GasComponent::new(
            GasLedger::UserTransaction,
            "compressed_entities.cleanup.index_touched_length",
            CE_INDEX_TOUCHED_LENGTH_CLEANUP_GAS,
            1,
        )
        .attributed_to("compressed_entities"),
    ]);
    let total_gas = calldata_stats
        .intrinsic_gas()
        .saturating_add(configured_base)
        .saturating_add(runtime_gas);

    let gate = measure_gate(fixture);
    let pairing = pairing_input();
    let mut observation =
        Observation::new([(GasLedger::UserTransaction, total_gas)], gas_components)
            .with_total_latency(full_path_ns)
            .with_calldata(calldata_stats)
            .with_setup_latency("crs_init", ms_to_ns(fixture.crs_init_ms))
            .with_setup_latency(
                "proof_generation_off_chain",
                ms_to_ns(fixture.proof_generation_ms),
            )
            .with_latency(
                "client.encrypt_payload",
                ms_to_ns(measure_encrypt_ms(&fixture.plaintext)),
            )
            .with_latency("client.abi_encode", ms_to_ns(measure_abi_ms(fixture)))
            .with_latency(
                "enclave.decrypt_payload",
                ms_to_ns(measure_enclave_decrypt_ms(fixture)),
            )
            .with_latency("enclave.process_offer", enclave_ns)
            .with_latency("chain.l2_gate", ms_to_ns(gate.wall_ms))
            .with_latency("chain.full_offer", full_path_ns)
            .with_latency(
                "chain.bn254_pair_reference",
                ms_to_ns(measure_pairing_ms(&pairing)),
            )
            .with_latency(
                "chain.ultrahonk_verify",
                ms_to_ns(measure_verify_ms(&fixture.proof)),
            )
            .with_postcondition("tribute.created", "true")
            .with_postcondition("tribute.id", tribute_id.to_string())
            .with_artifact(
                "tribute_proof",
                format!(
                    "keccak256:{:#x}",
                    alloy_primitives::keccak256(&fixture.proof)
                ),
            );
    observation.storage = aggregate_storage_trace(&trace);
    observation.events = aggregate_events(&ordered_events);
    observation.postconditions.insert(
        "fixture.plaintext_bytes".to_owned(),
        fixture.plaintext.len().to_string(),
    );
    observation.postconditions.insert(
        "fixture.ciphertext_bytes".to_owned(),
        fixture.cipher_text.len().to_string(),
    );
    observation.postconditions.insert(
        "fixture.proof_bytes".to_owned(),
        fixture.proof.len().to_string(),
    );
    Ok(observation)
}

fn storage_gas_components(trace: &[StorageTraceOperation]) -> Vec<GasComponent> {
    let mut grouped = BTreeMap::<(&'static str, StorageTraceKind), u64>::new();
    for operation in trace {
        *grouped
            .entry((module_name(operation.address), operation.kind))
            .or_default() += 1;
    }
    grouped
        .into_iter()
        .map(|((module, kind), count)| {
            let (suffix, per_operation) = match kind {
                StorageTraceKind::Read => ("read", WARM_STORAGE_READ_COST),
                StorageTraceKind::Write => ("write", SSTORE_RESET),
            };
            GasComponent::new(
                GasLedger::UserTransaction,
                format!("storage.{module}.{suffix}"),
                count.saturating_mul(per_operation),
                count,
            )
            .attributed_to(module)
        })
        .collect()
}

fn aggregate_storage_trace(trace: &[StorageTraceOperation]) -> Vec<StorageTraceEntry> {
    let mut grouped = BTreeMap::<(String, String, String, StorageOperationKind), u64>::new();
    for operation in trace {
        let kind = match operation.kind {
            StorageTraceKind::Read => StorageOperationKind::Read,
            StorageTraceKind::Write => StorageOperationKind::Write,
        };
        *grouped
            .entry((
                module_name(operation.address).to_owned(),
                format!("{:#x}", operation.address),
                format!("{:#x}", operation.slot),
                kind,
            ))
            .or_default() += 1;
    }
    grouped
        .into_iter()
        .map(
            |((module, address, slot, operation), count)| StorageTraceEntry {
                module,
                address,
                slot,
                operation,
                count,
                gas: count.saturating_mul(match operation {
                    StorageOperationKind::Read => WARM_STORAGE_READ_COST,
                    StorageOperationKind::Write => SSTORE_RESET,
                }),
            },
        )
        .collect()
}

fn aggregate_events(events: &[alloy_primitives::Log]) -> Vec<EventCount> {
    let mut grouped = BTreeMap::<(String, String), u64>::new();
    for event in events {
        let topic = event
            .data
            .topics()
            .first()
            .map_or_else(|| "none".to_owned(), |topic| format!("{topic:#x}"));
        *grouped
            .entry((format!("{:#x}", event.address), topic))
            .or_default() += 1;
    }
    grouped
        .into_iter()
        .map(|((emitter, event), count)| EventCount {
            emitter,
            event,
            count,
        })
        .collect()
}

fn module_name(address: Address) -> &'static str {
    match address {
        COMPRESSED_ENTITIES_ADDRESS => "compressed_entities",
        TRIBUTE_ADDRESS => "tribute",
        TRIBUTE_FACTORY_ADDRESS => "tribute_factory",
        METADOSIS_ADDRESS => "metadosis",
        ORACLE_ADDRESS => "oracle",
        L2_REGISTRY_ADDRESS => "l2_registry",
        AGENT_REWARD_ADDRESS => "agent_reward",
        _ => "other",
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn ms_to_ns(milliseconds: f64) -> u64 {
    (milliseconds * 1_000_000.0).round() as u64
}

/// Summarizes the ZK issuance cost decomposition of the Tribute creation
/// scenario: the frozen verifier charge actually configured for `offerTribute`
/// against the benchmark-calibrated alternative, both inside one ZK transaction.
///
/// Issuance is ZK-only, so no cross-scenario comparison is possible or wanted;
/// the calibration only re-prices the verifier charge within the same run.
#[must_use]
pub fn render_gas_policy(reports: &[ScenarioReport]) -> Option<String> {
    let zk = reports
        .iter()
        .find(|report| report.metadata.id == "tribute/create/zk/cold")?;
    let pairing_ns = zk
        .component_latency_ns
        .get("chain.bn254_pair_reference")?
        .median;
    let verifier_ns = zk
        .component_latency_ns
        .get("chain.ultrahonk_verify")?
        .median;
    if pairing_ns == 0 {
        return None;
    }
    let calibrated_verifier_gas =
        ((verifier_ns as f64 / pairing_ns as f64) * PAIRING_REFERENCE_GAS as f64).ceil() as u64;
    let recommended_verifier_gas = ceil_to(
        (calibrated_verifier_gas as f64 * VERIFIER_SAFETY_MULTIPLIER).ceil() as u64,
        GAS_ROUNDING,
    );
    let zk_total = zk.gas_totals[&GasLedger::UserTransaction];
    let configured_verifier_gas = gas_component(zk, "precompile.configured_base")?;
    let recommended_zk_total = zk_total
        .saturating_sub(configured_verifier_gas)
        .saturating_add(recommended_verifier_gas);
    let recommended_gas_limit = ceil_to(recommended_zk_total.saturating_mul(6) / 5, 100_000);
    // Signed so a recommendation below the configured charge is not masked as
    // a zero adjustment.
    let verifier_gas_adjustment =
        i128::from(recommended_verifier_gas) - i128::from(configured_verifier_gas);

    Some(format!(
        "\nTRIBUTE BENCHMARK-CALIBRATED GAS POLICY\n\
         BN254 reference:             {PAIRING_REFERENCE_GAS} gas at {:.3} ms median\n\
         UltraHonk verifier:          {:.3} ms median\n\
         Calibrated verifier gas:     {calibrated_verifier_gas}\n\
         Recommended verifier gas:    {recommended_verifier_gas} (1.5x, rounded)\n\
         Current configured verifier: {configured_verifier_gas}\n\
         Verifier gas adjustment:     {verifier_gas_adjustment}\n\
         Current ZK total:            {zk_total}\n\
         Recommended ZK total:        {recommended_zk_total}\n\
         Recommended tx gasLimit:     {recommended_gas_limit} (20% headroom)\n",
        pairing_ns as f64 / 1_000_000.0,
        verifier_ns as f64 / 1_000_000.0,
    ))
}

fn gas_component(report: &ScenarioReport, key: &str) -> Option<u64> {
    report
        .gas_components
        .iter()
        .find(|component| component.key == key)
        .map(|component| component.gas)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rewrite `fixtures/tribute_offer_proof.bin` from the deterministic
    /// statement in [`fixture_statement`].
    ///
    /// Barretenberg's proof words are not reproducible run to run — only the
    /// four public words are, which is why the file is frozen and only those
    /// are compared. Run this when the claim, the binding formula or the
    /// registered key changes:
    ///
    /// ```text
    /// cargo test -p outbe-protocol-benchmarks --lib -- --ignored \
    ///     regenerate_frozen_tribute_proof_fixture --nocapture
    /// ```
    #[test]
    #[ignore = "proves with the pinned Barretenberg CRS and rewrites the fixture"]
    fn regenerate_frozen_tribute_proof_fixture() {
        init_crs().expect("pinned CRS initializes");
        let vk = fixture_vk();
        let (claim, signer, binding_hash, mut rng) = fixture_statement();
        let path = empty_tree().unwrap().empty_inclusion_path(0);
        let (witness, public) = prove_tribute(&mut rng, &claim, &signer, binding_hash, &path)
            .expect("derive the tribute witness");
        let proof =
            ProofGenerator::<TributeDemo>::generate(&Barretenberg::default(), &witness, &public)
                .expect("generate the fixture proof");
        let combined = encode_combined_proof(&public_words(&public), &proof.proof, vk)
            .expect("encode the combined proof");
        assert_eq!(
            combined.len(),
            combined_len(vk, PUBLIC_INPUT_COUNT).unwrap()
        );
        assert!(Barretenberg::default()
            .verify_combined(vk, &combined)
            .expect("the regenerated proof verifies under the registered key"));

        let decoded: TributePublicInputs = decode_public_inputs(&combined, vk)
            .unwrap()
            .try_into()
            .unwrap();
        println!("owner        {:#x}", decoded.owner);
        println!("nft_hash     {:#x}", decoded.nft_hash);
        println!("binding_hash {:#x}", decoded.binding_hash);
        println!("merkle_root  {:#x}", decoded.merkle_root);

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/tribute_offer_proof.bin"
        );
        std::fs::write(path, &combined).expect("write the frozen fixture");
        println!("wrote {} bytes to {path}", combined.len());
    }
}
