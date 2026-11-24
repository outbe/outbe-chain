use alloy_sol_types::SolError;
use commonware_codec::Encode;
use commonware_cryptography::bls12381::primitives::{
    ops::{self, sign_message},
    variant::MinSig,
};
use outbe_agentreward::AgentRewardContract;
use outbe_l2registry::L2RegistryContract;
use outbe_primitives::storage::{CycleLifecyclePurpose, MutationPurposeTag};
use outbe_system_common::time::{date_key_to_utc_timestamp, WorldwideDay};
use outbe_zk::demo_offer::COMBINED_LEN as DEMO_COMBINED_LEN;

use super::*;
use crate::offer::{issue_nod, IssueNodInput, OfferError};

const HOST_CHAIN_ID: u64 = outbe_primitives::chain::DEVNET_CHAIN_ID;
const L2_CHAIN_ID: u64 = 0xdead;
const CALLER: Address = Address::repeat_byte(0x77);
const TARGET_WWD: WorldwideDay = WorldwideDay::new(20_260_802);
const OFFER_DAY: u32 = 20_260_803;
const REWARD_WALLET: Address = Address::repeat_byte(0x71);
const REWARD_SRA: Address = Address::repeat_byte(0x72);

fn offer(zk_merkle_root: &[u8], signature: &[u8]) -> IssueNodInput {
    IssueNodInput {
        caller: CALLER,
        cipher_text: Bytes::new(),
        nonce: Bytes::new(),
        ephemeral_pubkey: U256::ZERO,
        worldwide_day: TARGET_WWD,
        issuance_currency: 840,
        reference_currency: 840,
        zk_proof: Bytes::new(),
        l2_chain_id: u32::try_from(L2_CHAIN_ID).unwrap(),
        circuit_version: "1.1.0".to_owned(),
        zk_merkle_root: Bytes::copy_from_slice(zk_merkle_root),
        signature: Bytes::copy_from_slice(signature),
    }
}

fn dummy_proof(root: [u8; 32]) -> Bytes {
    let mut proof = Vec::with_capacity(DEMO_COMBINED_LEN);
    proof.extend_from_slice(&4u32.to_be_bytes());
    proof.extend_from_slice(&[0x01; 32]);
    proof.extend_from_slice(&[0x02; 32]);
    proof.extend_from_slice(&[0x03; 32]);
    proof.extend_from_slice(&root);
    proof.resize(DEMO_COMBINED_LEN, 0);
    proof.into()
}

fn network_key() -> (
    commonware_cryptography::bls12381::primitives::group::Private,
    Vec<u8>,
) {
    let mut rng =
        <rand_commonware::rngs::StdRng as rand_commonware::SeedableRng>::from_seed([0x5a; 32]);
    let (private, public) = ops::keypair::<_, MinSig>(&mut rng);
    (
        private,
        outbe_l2registry::public_key::encode(&public)
            .unwrap()
            .to_vec(),
    )
}

fn sign_root(
    private: &commonware_cryptography::bls12381::primitives::group::Private,
    root: &[u8],
) -> Vec<u8> {
    sign_message::<MinSig>(
        private,
        outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE,
        root,
    )
    .encode()
    .to_vec()
}

/// A signed root and a well-framed dummy proof: an offer that reaches the business checks.
/// Its proof never reaches the verifier.
fn signed_gate_offer(storage: &StorageHandle<'_>) -> IssueNodInput {
    let (private, public) = network_key();
    L2RegistryContract::new(storage.clone())
        .register_network(L2_CHAIN_ID, CALLER, &public)
        .unwrap();
    let root = [0x04; 32];
    let mut input = offer(&root, &sign_root(&private, &root));
    input.zk_proof = dummy_proof(root);
    input
}

fn assert_reverts(error: PrecompileError, expected: impl Into<PrecompileError>) {
    assert_eq!(error.to_string(), expected.into().to_string());
}

#[test]
fn offer_rejects_invalid_l2_signature() {
    let (private, public) = network_key();
    let root = [0x04; 32];
    let mut provider = HashMapStorageProvider::new(HOST_CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = L2RegistryContract::new(storage.clone());
        registry
            .register_network(L2_CHAIN_ID, CALLER, &public)
            .unwrap();
        assert_reverts(
            issue_nod(&storage, offer(&root, &[])).unwrap_err(),
            outbe_l2registry::errors::L2RegistryError::InvalidZkSignature,
        );

        let signature = sign_root(&private, &root);
        // A signed root selects no missing or newer circuit version.
        for version in ["", "1.2.0"] {
            let mut wrong_version = offer(&root, &signature);
            wrong_version.zk_proof = dummy_proof(root);
            wrong_version.circuit_version = version.to_owned();
            assert_reverts(
                issue_nod(&storage, wrong_version).unwrap_err(),
                OfferError::UnknownCircuitVersion {
                    chain_id: u32::try_from(L2_CHAIN_ID).unwrap(),
                    version: version.to_owned(),
                },
            );
        }
        // The submitter need not be the registered network operator.
        let mut valid_gate = offer(&root, &signature);
        valid_gate.caller = Address::repeat_byte(0x88);
        valid_gate.zk_proof = dummy_proof(root);
        assert_reverts(
            issue_nod(&storage, valid_gate).unwrap_err(),
            OfferError::WorldwideDayNotOffering {
                worldwide_day: TARGET_WWD,
            },
        );

        // A registration on another chain cannot authorize an offer for this one.
        registry.remove_network(CALLER, L2_CHAIN_ID).unwrap();
        registry.register_network(4242, CALLER, &public).unwrap();
        let mut wrong_chain = offer(&root, &signature);
        wrong_chain.zk_proof = dummy_proof(root);
        assert_reverts(
            issue_nod(&storage, wrong_chain).unwrap_err(),
            outbe_l2registry::errors::L2RegistryError::NetworkNotRegistered {
                chain_id: L2_CHAIN_ID,
            },
        );
    });
}

#[test]
fn registered_network_requires_proof_and_matching_public_root() {
    let (private, public) = network_key();
    let root = [0x04; 32];
    let signature = sign_root(&private, &root);
    let mut provider = HashMapStorageProvider::new(HOST_CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        L2RegistryContract::new(storage.clone())
            .register_network(L2_CHAIN_ID, CALLER, &public)
            .unwrap();
        assert_reverts(
            issue_nod(&storage, offer(&root, &signature)).unwrap_err(),
            OfferError::ZkProofRequired,
        );
        let mut wrong_root = offer(&root, &signature);
        wrong_root.zk_proof = dummy_proof([0x24; 32]);
        assert_reverts(
            issue_nod(&storage, wrong_root).unwrap_err(),
            OfferError::ZkPublicInputMismatch {
                field: "merkle_root",
            },
        );
    });
}

#[test]
fn unregistered_chains_cannot_offer() {
    let mut provider = HashMapStorageProvider::new(HOST_CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        assert_reverts(
            issue_nod(&storage, offer(&[], &[])).unwrap_err(),
            outbe_l2registry::errors::L2RegistryError::NetworkNotRegistered {
                chain_id: L2_CHAIN_ID,
            },
        );
    });
}

/// The day and the currencies are cleartext arguments, so a bad one costs no enclave
/// round trip. No enclave is installed here: reaching it would fail differently.
#[test]
fn host_rejects_an_invalid_calendar_day_before_the_enclave() {
    let mut provider = HashMapStorageProvider::new(HOST_CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let mut bad_day = signed_gate_offer(&storage);
        bad_day.worldwide_day = WorldwideDay::new(20_250_230);
        assert_reverts(
            issue_nod(&storage, bad_day).unwrap_err(),
            OfferError::InvalidWorldwideDay {
                worldwide_day: WorldwideDay::new(20_250_230),
            },
        );
    });
}

/// The day check runs before pricing: an unregistered currency on a closed day still
/// reports the day.
#[test]
fn host_rejects_a_non_offering_day_before_pricing() {
    let mut provider = HashMapStorageProvider::new(HOST_CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let mut unpriced = signed_gate_offer(&storage);
        unpriced.issuance_currency = 999;
        assert_reverts(
            issue_nod(&storage, unpriced).unwrap_err(),
            OfferError::WorldwideDayNotOffering {
                worldwide_day: TARGET_WWD,
            },
        );
    });
}

#[test]
fn agent_reward_addresses_come_in_pairs() {
    use crate::offer::validate_agent_reward_addresses as validate;
    let wallets = vec!["0x1111111111111111111111111111111111111111".to_string()];
    let sras = vec!["0x2222222222222222222222222222222222222222".to_string()];
    assert!(validate(&[], &[]).is_ok());
    assert!(validate(&wallets, &sras).is_ok());
    assert!(validate(&wallets, &[]).is_err());
    assert!(validate(&[], &sras).is_err());
    assert!(validate(&["not_a_valid_address".to_string()], &sras).is_err());
}

/// Opens `wwd` and prices COEN/840 with a 24-hour VWAP at every hour.
fn seed_offer_world(storage: &StorageHandle<'_>, wwd: WorldwideDay) {
    let timestamp = u64::try_from(storage.timestamp().unwrap()).unwrap();
    let context = outbe_primitives::block::BlockRuntimeContext::new(
        outbe_primitives::block::BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: storage.block_number().unwrap(),
            timestamp,
            chain_id: HOST_CHAIN_ID,
            genesis_hash: B256::ZERO,
            genesis_timestamp: timestamp,
            proposer: Address::ZERO,
            validators: Vec::new(),
        },
        storage.clone(),
    );
    let binding = outbe_system_common::metadosis_bindings::metadosis_process_ready_binding(
        HOST_CHAIN_ID,
        storage.block_number().unwrap(),
        storage.now_secs().unwrap(),
    );
    storage
        .clone()
        .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
            outbe_worldwideday::open_worldwide_day_for_date(&context, wwd)
        })
        .unwrap();
    let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    outbe_oracle::genesis::init_from_genesis(
        &mut oracle,
        &outbe_oracle::genesis::OracleGenesisConfig::default_config(),
    )
    .unwrap();
    outbe_oracle::test_support::seed_covered_trailing_window(
        storage,
        outbe_oracle::api::AddressPair::new_coen_to(840),
        U256::from(100),
        U256::ONE,
    )
    .unwrap();
}

fn offer_world(network_key: &[u8]) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(HOST_CHAIN_ID);
    provider.set_timestamp(U256::from(
        date_key_to_utc_timestamp(OFFER_DAY).unwrap() + 43_200,
    ));
    provider.enable_scoped_mutation_frame(MutationPurposeTag::CycleLifecycle);
    StorageHandle::enter(&mut provider, |storage| {
        seed_offer_world(&storage, TARGET_WWD);
        L2RegistryContract::new(storage)
            .register_network(L2_CHAIN_ID, CALLER, network_key)
            .unwrap();
    });
    provider
}

/// The offer payload of [`CALLER`], sealed to the stand-in enclave's offer key.
fn sealed_payload(su_hashes: &[B256]) -> (Bytes, Bytes, U256) {
    sealed_offer(CALLER, ("100", "0"), su_hashes)
}

/// The offer payload of `owner` spending `amount_base.amount_micro`, sealed to the
/// stand-in enclave's offer key.
fn sealed_offer(
    owner: Address,
    (amount_base, amount_micro): (&str, &str),
    su_hashes: &[B256],
) -> (Bytes, Bytes, U256) {
    use x25519_dalek::{PublicKey, StaticSecret};
    let payload = serde_json::to_vec(&serde_json::json!({
        "creator": format!("{owner:#x}"),
        "offer_draft_id": format!("{:#x}", B256::with_last_byte(0x11)),
        "amount_base": amount_base,
        "amount_micro": amount_micro,
        "su_hashes": su_hashes.iter().map(|hash| format!("{hash:#x}")).collect::<Vec<_>>(),
        "wallet_addresses": [format!("{REWARD_WALLET:#x}")],
        "sra_addresses": [format!("{REWARD_SRA:#x}")],
    }))
    .unwrap();
    let offer_key =
        PublicKey::from(&StaticSecret::from(outbe_nod::test_support::OFFER_SECRET)).to_bytes();
    let (cipher, nonce, ephemeral) =
        outbe_core_common::confidential_tee::encrypt_offer(&offer_key, &payload).unwrap();
    (
        Bytes::from(cipher),
        Bytes::from(nonce.to_vec()),
        U256::from_be_bytes(ephemeral),
    )
}

/// The enclave refuses an offer that lists no SU hash: no Nod, no mark, no reward.
#[test]
fn an_empty_su_hash_list_rejects_before_any_write() {
    let _enclave = outbe_nod::test_support::scope();
    let (private, public) = network_key();
    let root = [0x04; 32];
    let mut provider = offer_world(&public);
    let before = provider.storage.clone();
    let events_before = provider.get_ordered_events().to_vec();
    let (cipher_text, nonce, ephemeral_pubkey) = sealed_payload(&[]);
    let input = IssueNodInput {
        cipher_text,
        nonce,
        ephemeral_pubkey,
        zk_proof: dummy_proof(root),
        ..offer(&root, &sign_root(&private, &root))
    };
    StorageHandle::enter(&mut provider, |storage| {
        let error = storage
            .with_checkpoint(|| issue_nod(&storage, input))
            .unwrap_err();
        let PrecompileError::RevertBytes(bytes) = error else {
            panic!("expected the enclave refusal, got {error:?}");
        };
        let refusal = INodFactory::EnclaveRejected::abi_decode(&bytes).unwrap();
        assert!(refusal.reason.contains("su_hashes"), "{}", refusal.reason);
    });
    assert_eq!(provider.storage, before, "an empty SU list wrote state");
    assert_eq!(provider.get_ordered_events(), events_before);
}

#[test]
#[ignore = "requires the pinned Barretenberg CRS"]
fn real_zk_offer_issues_its_nod_once_and_rolls_back_failures() {
    let proof = include_bytes!(
        "../../../../../testing/protocol-benchmarks/fixtures/offer_demo_offer_v1.bin"
    );
    assert_real_zk_offer(proof);
}

fn assert_real_zk_offer(proof: &[u8]) {
    let _enclave = outbe_nod::test_support::scope();
    outbe_zk_backend::barretenberg::BarretenbergHandler::init().unwrap();
    let merkle_root =
        B256::from(outbe_zk::wire::decode_public_words::<4>(proof, proof.len()).unwrap()[3]);
    let (private, public) = network_key();
    let signature = sign_root(&private, merkle_root.as_slice());
    let su_hash = B256::with_last_byte(0x22);
    let make_offer = || {
        let (cipher_text, nonce, ephemeral_pubkey) = sealed_payload(&[su_hash]);
        IssueNodInput {
            cipher_text,
            nonce,
            ephemeral_pubkey,
            zk_proof: Bytes::copy_from_slice(proof),
            ..offer(merkle_root.as_slice(), &signature)
        }
    };
    let mut provider = offer_world(&public);
    let before_offer = provider.storage.clone();
    provider.fail_after_mutation_at(usize::MAX);
    let nod_id = StorageHandle::enter(&mut provider, |storage| {
        let nod_id = issue_nod(&storage, make_offer()).unwrap();
        let item = nod_api::get_item(&storage, nod_id).unwrap().unwrap();
        let wwd = outbe_worldwideday::worldwide_day(storage.clone(), TARGET_WWD)
            .unwrap()
            .unwrap();
        assert_eq!(item.owner, CALLER);
        assert_eq!(item.series_id.day(), wwd.day);
        let now = storage.now_secs().unwrap();
        assert_eq!(
            u64::from(item.sub_series_id),
            (now - now % 3_600 - wwd.offerings_start) / 3_600
        );
        let totals = outbe_worldwideday::worldwide_day_totals(storage.clone(), TARGET_WWD).unwrap();
        assert_eq!(totals.nod_count, 1);
        assert!(NodContract::new(storage.clone())
            .has_used_su_hash(su_hash)
            .unwrap());
        nod_id
    });
    let mutations = provider.clear_mutation_failure();
    StorageHandle::enter(&mut provider, |storage| {
        let replay = issue_nod(&storage, make_offer()).unwrap_err();
        assert!(matches!(replay, PrecompileError::RevertBytes(_)));
        let rewards = AgentRewardContract::new(storage);
        assert_eq!(
            rewards.get_all_waa_counts(OFFER_DAY.into()).unwrap(),
            vec![(REWARD_WALLET, 1)]
        );
        assert_eq!(
            rewards.get_all_sra_counts(OFFER_DAY.into()).unwrap(),
            vec![(REWARD_SRA, 1)]
        );
        assert!(rewards.get_all_waa_counts(TARGET_WWD).unwrap().is_empty());
    });

    // A failure at the last write of the offer rolls the whole offer back.
    provider.storage = before_offer.clone();
    provider.fail_after_mutation_at(mutations - 1);
    StorageHandle::enter(&mut provider, |storage| {
        storage
            .with_checkpoint(|| issue_nod(&storage, make_offer()))
            .unwrap_err();
    });
    assert_eq!(provider.clear_mutation_failure(), mutations);
    assert_eq!(
        provider.storage, before_offer,
        "a failed offer leaked state"
    );

    // An SRA counter overflow after every other write rolls the offer back too.
    let reward_day = WorldwideDay::new(OFFER_DAY);
    let count_key = AgentRewardContract::offer_count_key(reward_day, REWARD_SRA);
    let index_key = AgentRewardContract::address_index_key(reward_day, 0);
    StorageHandle::enter(&mut provider, |storage| {
        let rewards = AgentRewardContract::new(storage);
        rewards.sra_addresses.write(&index_key, REWARD_SRA).unwrap();
        rewards.sra_address_count.write(&reward_day, 1).unwrap();
        rewards
            .sra_offer_counts
            .write(&count_key, u64::MAX)
            .unwrap();
        rewards
            .sra_total_count
            .write(&reward_day, u64::MAX)
            .unwrap();
    });
    let before_overflow = provider.storage.clone();
    let events_before_overflow = provider.get_ordered_events().to_vec();
    StorageHandle::enter(&mut provider, |storage| {
        let error = storage
            .with_checkpoint(|| issue_nod(&storage, make_offer()))
            .unwrap_err();
        let PrecompileError::RevertBytes(bytes) = error else {
            panic!("expected the counter-overflow revert, got {error:?}");
        };
        assert_eq!(
            bytes.as_ref(),
            outbe_agentreward::precompile::IAgentReward::OfferCounterOverflow {}.abi_encode()
        );
        assert!(nod_api::get_item(&storage, nod_id).unwrap().is_none());
        assert!(!NodContract::new(storage.clone())
            .has_used_su_hash(su_hash)
            .unwrap());
    });
    assert_eq!(
        provider.storage, before_overflow,
        "the overflow leaked state"
    );
    assert_eq!(provider.get_ordered_events(), events_before_overflow);
}

// --- The offer past its proof -----------------------------------------------------------
//
// A real proof needs the pinned Barretenberg CRS, which the ignored test above loads. These
// tests run every other step of `issueNod` in its order: pricing, the sub-series terms, the
// enclave issue, the claims and the writes.

fn offer_of(owner: Address, issuance_currency: u16, amount: (&str, &str)) -> IssueNodInput {
    let (cipher_text, nonce, ephemeral_pubkey) = sealed_offer(
        owner,
        amount,
        &[B256::from(U256::from_be_bytes(owner.into_word().0))],
    );
    IssueNodInput {
        caller: owner,
        cipher_text,
        nonce,
        ephemeral_pubkey,
        issuance_currency,
        ..offer(&[], &[])
    }
}

fn zk_context() -> outbe_nod::offer::protocol::OfferZkContext {
    outbe_nod::offer::protocol::OfferZkContext {
        derived_owner: B256::ZERO,
        chain_id: HOST_CHAIN_ID,
        l2_chain_id: L2_CHAIN_ID,
    }
}

/// `issueNod` of `input` from its pricing on, with `scurve` in place of the S-curve of the
/// hour when given.
fn issue_past_proof(
    storage: &StorageHandle<'_>,
    input: &IssueNodInput,
    scurve: Option<U256>,
) -> Result<U256, PrecompileError> {
    let (wwd, mut pricing) = crate::offer::pricing(storage, input)?;
    if let Some(scurve) = scurve {
        pricing.reference_scurve_minor = scurve;
    }
    let now = storage.now_secs()?;
    let (sub_series_id, entry) = crate::offer::offer_terms(storage, &wwd, input, now, &pricing)?;
    let request = crate::offer::offer_request(input, (wwd.day, entry), &pricing, zk_context());
    let (nod, result) = crate::offer::issued_nod(nod_api::issue_at_offer(storage, request)?)?;
    let claims = crate::offer::offer_claims(&result)?;
    crate::offer::record_offer(
        storage,
        input.worldwide_day,
        &nod,
        (now, sub_series_id),
        &claims,
    )
}

fn offerings_start(storage: &StorageHandle<'_>) -> u64 {
    outbe_worldwideday::worldwide_day(storage.clone(), TARGET_WWD)
        .unwrap()
        .unwrap()
        .offerings_start
}

/// Moves the block to `minutes` past the start of offer hour `hour`. With `rate`, prices
/// COEN/840 and COEN/978 at it for the 24 hours before the hour.
fn at_offer_hour(storage: &StorageHandle<'_>, (hour, minutes): (u64, u64), rate: Option<u64>) {
    let at = offerings_start(storage) + hour * 3_600 + minutes * 60;
    storage.set_block_timestamp(U256::from(at)).unwrap();
    if let Some(rate) = rate {
        for iso in [840, 978] {
            let pair = outbe_oracle::api::AddressPair::new_coen_to(iso);
            if outbe_oracle::schema::OracleContract::new(storage.clone())
                .pair_index_of(pair)
                .unwrap()
                == 0
            {
                outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
            }
            outbe_oracle::test_support::seed_covered_trailing_window(
                storage,
                pair,
                U256::from(rate),
                U256::ONE,
            )
            .unwrap();
        }
    }
}

fn reference_vwap(storage: &StorageHandle<'_>) -> U256 {
    outbe_oracle::api::tribute_pricing_inputs(storage.clone(), 840, 840)
        .unwrap()
        .unwrap()
        .reference_vwap_minor
}

/// The offering window of [`TARGET_WWD`] before it opens, with no price yet.
fn priced_world() -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(HOST_CHAIN_ID);
    provider.set_timestamp(U256::from(
        date_key_to_utc_timestamp(OFFER_DAY - 1).unwrap() + 9 * 3_600,
    ));
    provider.enable_scoped_mutation_frame(MutationPurposeTag::CycleLifecycle);
    StorageHandle::enter(&mut provider, |storage| {
        let timestamp = storage.now_secs().unwrap();
        let context = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: storage.block_number().unwrap(),
                timestamp,
                chain_id: HOST_CHAIN_ID,
                genesis_hash: B256::ZERO,
                genesis_timestamp: timestamp,
                proposer: Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        let binding = outbe_system_common::metadosis_bindings::metadosis_process_ready_binding(
            HOST_CHAIN_ID,
            storage.block_number().unwrap(),
            timestamp,
        );
        storage
            .clone()
            .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
                outbe_worldwideday::open_worldwide_day_for_date(&context, TARGET_WWD)
            })
            .unwrap();
        outbe_oracle::genesis::init_from_genesis(
            &mut outbe_oracle::schema::OracleContract::new(storage.clone()),
            &outbe_oracle::genesis::OracleGenesisConfig::default_config(),
        )
        .unwrap();
    });
    provider
}

fn nominal_and_load(nod_id: U256, storage: &StorageHandle<'_>) -> (U256, U256) {
    let item = nod_api::get_item(storage, nod_id).unwrap().unwrap();
    let key = outbe_nod::test_support::state_key();
    (
        outbe_nod::nod_encryption::decrypt_nod_nominal(&key, &item.encrypted).unwrap(),
        outbe_nod::nod_encryption::decrypt_nod(&key, &item.encrypted).unwrap(),
    )
}

fn call_price(storage: &StorageHandle<'_>, nod_id: U256) -> U256 {
    let item = nod_api::get_item(storage, nod_id).unwrap().unwrap();
    NodContract::new(storage.clone())
        .read_call_terms(item.group_key)
        .unwrap()
        .call_price_minor
}

fn marked_up(storage: &StorageHandle<'_>, entry: U256) -> U256 {
    let params = outbe_nod::config::read(storage).unwrap();
    outbe_core_common::markup::marked_up(entry, params.call_rate).unwrap()
}

#[test]
fn issue_nod_at_the_offer_gives_all_terms() {
    let _enclave = outbe_nod::test_support::scope();
    let mut provider = priced_world();
    StorageHandle::enter(&mut provider, |storage| {
        at_offer_hour(&storage, (3, 20), Some(100));
        let entry = reference_vwap(&storage);
        let nod_id =
            issue_past_proof(&storage, &offer_of(CALLER, 840, ("100", "0")), None).unwrap();
        let item = nod_api::get_item(&storage, nod_id).unwrap().unwrap();
        let wwd = outbe_worldwideday::worldwide_day(storage.clone(), TARGET_WWD)
            .unwrap()
            .unwrap();
        assert_eq!(item.owner, CALLER);
        assert_eq!(item.series_id.day(), wwd.day);
        assert_eq!(item.sub_series_id, 3);
        assert!(!item.is_settled);
        assert_eq!(item.issued_at, storage.now_secs().unwrap());
        assert_eq!(item.entry_price_minor(), entry);
        assert_eq!(call_price(&storage, nod_id), marked_up(&storage, entry));
        let (nominal, load) = nominal_and_load(nod_id, &storage);
        assert!(!nominal.is_zero());
        assert_eq!(load, outbe_nod::offer::issue::nod_load_of(nominal));
        assert_eq!(
            outbe_worldwideday::worldwide_day_totals(storage.clone(), TARGET_WWD)
                .unwrap()
                .nod_count,
            1
        );
    });
}

#[test]
fn nod_load_is_32_percent_of_the_nominal() {
    let _enclave = outbe_nod::test_support::scope();
    let mut provider = priced_world();
    StorageHandle::enter(&mut provider, |storage| {
        at_offer_hour(&storage, (1, 0), Some(100));
        for (index, amount) in [("100", "0"), ("3", "141592"), ("0", "999999")]
            .into_iter()
            .enumerate()
        {
            let owner = Address::repeat_byte(0x30 + u8::try_from(index).unwrap());
            let nod_id = issue_past_proof(&storage, &offer_of(owner, 840, amount), None).unwrap();
            let (nominal, load) = nominal_and_load(nod_id, &storage);
            assert_eq!(
                load,
                nominal * U256::from(32) / U256::from(100),
                "{amount:?}"
            );
        }
    });
}

/// A Nod that cannot get a load reverts the offer: no VWAP at the hour, or a load below one
/// minor unit. Nothing is used up, and a later offer of the owner passes.
#[test]
fn issue_nod_with_no_load_reverts() {
    let _enclave = outbe_nod::test_support::scope();
    let unpriced = PrecompileError::from(OfferError::CurrencyPriceUnavailable {
        worldwide_day: TARGET_WWD,
    });
    let dust = PrecompileError::from(OfferError::LoadBelowOneMinorUnit);
    // 0.000001 at 0.333334 per COEN is a nominal of 2 minor units and a load of 0; at 2
    // per COEN the nominal itself rounds to 0.
    for (currency, amount, rate, expected) in [
        (826, ("100", "0"), 100, unpriced.to_string()),
        (840, ("0", "1"), 333_334, dust.to_string()),
        (840, ("0", "1"), 2_000_000, dust.to_string()),
    ] {
        let mut provider = priced_world();
        StorageHandle::enter(&mut provider, |storage| {
            at_offer_hour(&storage, (2, 0), Some(rate));
            // COEN/826 is listed but has no trade in the 24 hours before the offer hour.
            outbe_oracle::api::register_pair(
                storage.clone(),
                outbe_oracle::api::AddressPair::new_coen_to(826),
            )
            .unwrap();
        });
        let before = provider.storage.clone();
        let events = provider.get_ordered_events().to_vec();
        StorageHandle::enter(&mut provider, |storage| {
            let input = offer_of(CALLER, currency, amount);
            let error = storage
                .with_checkpoint(|| issue_past_proof(&storage, &input, None))
                .unwrap_err();
            assert_eq!(error.to_string(), expected);
        });
        assert_eq!(provider.storage, before, "a refused offer wrote state");
        assert_eq!(provider.get_ordered_events(), events);
        StorageHandle::enter(&mut provider, |storage| {
            issue_past_proof(&storage, &offer_of(CALLER, 840, ("100", "0")), None).unwrap();
        });
    }
}

/// The nominal and the load leave the enclave only encrypted: no read and no event holds
/// them in clear.
#[test]
fn nod_nominal_is_encrypted() {
    let _enclave = outbe_nod::test_support::scope();
    let mut provider = priced_world();
    let (nod_id, nominal, load) = StorageHandle::enter(&mut provider, |storage| {
        at_offer_hour(&storage, (0, 10), Some(100));
        let nod_id =
            issue_past_proof(&storage, &offer_of(CALLER, 840, ("100", "0")), None).unwrap();
        let (nominal, load) = nominal_and_load(nod_id, &storage);
        (nod_id, nominal, load)
    });
    let data = public_nod_data(&mut World { provider }, nod_id);
    let encoded = alloy_sol_types::SolValue::abi_encode(&data);
    for clear in [nominal, load] {
        let word = clear.to_be_bytes::<32>();
        assert!(!encoded.windows(32).any(|window| window == word));
    }
    assert_eq!(
        data.encryptedNominalAmount.len(),
        outbe_core_common::nod_encryption::NOD_BLOB_LEN
    );
}

#[test]
fn nod_nominal_stays_out_of_every_event() {
    let _enclave = outbe_nod::test_support::scope();
    let mut provider = priced_world();
    let (nominal, load) = StorageHandle::enter(&mut provider, |storage| {
        at_offer_hour(&storage, (0, 10), Some(100));
        let nod_id =
            issue_past_proof(&storage, &offer_of(CALLER, 840, ("100", "0")), None).unwrap();
        nominal_and_load(nod_id, &storage)
    });
    for clear in [nominal, load] {
        let word = clear.to_be_bytes::<32>();
        for event in provider.get_ordered_events() {
            assert!(!event.data.data.windows(32).any(|window| window == word));
            assert!(!event.data.topics().iter().any(|topic| topic.0 == word));
        }
    }
}

#[test]
fn nod_sub_series_for_each_offer_hour() {
    use crate::offer::offer_hour;
    let start = 1_800_000_000;
    assert_eq!(offer_hour(start, start).unwrap(), 0);
    assert_eq!(offer_hour(start + 49 * 3_600 + 3_599, start).unwrap(), 49);
    assert_eq!(offer_hour(start + 4 * 3_600 + 1_800, start).unwrap(), 4);
    assert_eq!(offer_hour(start + 5 * 3_600 + 600, start).unwrap(), 5);

    let _enclave = outbe_nod::test_support::scope();
    let mut provider = priced_world();
    StorageHandle::enter(&mut provider, |storage| {
        at_offer_hour(&storage, (4, 30), Some(100));
        let first_entry = reference_vwap(&storage);
        let first = issue_past_proof(
            &storage,
            &offer_of(Address::repeat_byte(0x41), 840, ("100", "0")),
            None,
        )
        .unwrap();
        at_offer_hour(&storage, (5, 10), Some(300));
        let second_entry = reference_vwap(&storage);
        assert_ne!(first_entry, second_entry);
        let second = issue_past_proof(
            &storage,
            &offer_of(Address::repeat_byte(0x42), 840, ("100", "0")),
            None,
        )
        .unwrap();
        at_offer_hour(&storage, (5, 50), None);
        let same_hour = issue_past_proof(
            &storage,
            &offer_of(Address::repeat_byte(0x43), 840, ("100", "0")),
            None,
        )
        .unwrap();

        let item = |nod_id| nod_api::get_item(&storage, nod_id).unwrap().unwrap();
        assert_eq!(
            (item(first).sub_series_id, item(first).entry_price_minor()),
            (4, first_entry)
        );
        assert_eq!(
            (item(second).sub_series_id, item(second).entry_price_minor()),
            (5, second_entry)
        );
        assert_eq!(
            (
                item(same_hour).sub_series_id,
                item(same_hour).entry_price_minor()
            ),
            (5, second_entry),
            "a sub-series keeps the price of its first Nod"
        );
        assert_eq!(
            call_price(&storage, first),
            marked_up(&storage, first_entry)
        );
        assert_eq!(
            call_price(&storage, second),
            marked_up(&storage, second_entry)
        );
        assert_eq!(item(second).group_key, item(same_hour).group_key);
        assert_ne!(item(first).group_key, item(second).group_key);
    });
}

#[test]
fn nod_series_opens_with_its_first_nod() {
    let _enclave = outbe_nod::test_support::scope();
    let mut provider = priced_world();
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            offerings_start(&storage),
            date_key_to_utc_timestamp(OFFER_DAY - 1).unwrap() + 10 * 3_600,
            "the series of E starts at 10:00 E-1"
        );
        at_offer_hour(&storage, (2, 0), Some(100));
        for (owner, currency) in [(0x51, 840), (0x52, 840), (0x53, 978)] {
            issue_past_proof(
                &storage,
                &offer_of(Address::repeat_byte(owner), currency, ("100", "0")),
                None,
            )
            .unwrap();
        }
    });
    let opened: Vec<_> = provider
        .get_events(NOD_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| INodFactory::NodSeriesOpened::decode_log_data(log).ok())
        .map(|event| {
            (
                event.subSeriesId,
                event.issuanceCurrency,
                event.referenceCurrency,
            )
        })
        .collect();
    assert_eq!(opened, [(2, 840, 840), (2, 978, 840)]);
}

/// A Nod keeps the 24-hour VWAP of its offer hour as its entry price, also when the S-curve
/// of the hour is above it, and no VWAP of its emission day is needed.
#[test]
fn nod_keeps_the_price_of_its_offer_hour() {
    let _enclave = outbe_nod::test_support::scope();
    let mut provider = priced_world();
    StorageHandle::enter(&mut provider, |storage| {
        at_offer_hour(&storage, (6, 0), Some(100));
        let vwap = reference_vwap(&storage);
        let plain = issue_past_proof(
            &storage,
            &offer_of(Address::repeat_byte(0x61), 840, ("100", "0")),
            None,
        )
        .unwrap();
        let curved = issue_past_proof(
            &storage,
            &offer_of(Address::repeat_byte(0x62), 840, ("100", "0")),
            Some(vwap * U256::from(4)),
        )
        .unwrap();
        for nod_id in [plain, curved] {
            assert_eq!(
                nod_api::get_item(&storage, nod_id)
                    .unwrap()
                    .unwrap()
                    .entry_price_minor(),
                vwap
            );
        }
        assert!(
            nominal_and_load(curved, &storage).0 < nominal_and_load(plain, &storage).0,
            "the S-curve prices the nominal, not the entry"
        );
        let emission_day = outbe_worldwideday::worldwide_day(storage.clone(), TARGET_WWD)
            .unwrap()
            .unwrap()
            .day;
        assert!(
            outbe_oracle::api::get_utc_day_vwap_for_iso(storage.clone(), emission_day, 840)
                .unwrap()
                .is_none()
        );
    });
}

/// A failure at each write of an offer reverts all of it, and the retry issues the Nod once.
#[test]
fn issue_nod_is_atomic_at_each_write() {
    let _enclave = outbe_nod::test_support::scope();
    let input = || offer_of(CALLER, 840, ("100", "0"));
    let world = || {
        let mut provider = priced_world();
        StorageHandle::enter(&mut provider, |storage| {
            at_offer_hour(&storage, (1, 0), Some(100))
        });
        provider
    };
    let mut control = world();
    control.fail_after_mutation_at(usize::MAX);
    StorageHandle::enter(&mut control, |storage| {
        issue_past_proof(&storage, &input(), None).unwrap();
    });
    let writes = control.clear_mutation_failure();
    assert!(writes > 0);
    for write in 0..writes {
        let mut provider = world();
        let before = provider.storage.clone();
        let events = provider.get_ordered_events().to_vec();
        provider.fail_after_mutation_at(write);
        StorageHandle::enter(&mut provider, |storage| {
            storage
                .with_checkpoint(|| issue_past_proof(&storage, &input(), None))
                .unwrap_err();
        });
        provider.clear_mutation_failure();
        assert_eq!(provider.storage, before, "write {write}");
        assert_eq!(provider.get_ordered_events(), events, "write {write}");
        StorageHandle::enter(&mut provider, |storage| {
            issue_past_proof(&storage, &input(), None).unwrap();
            assert_reverts(
                storage
                    .with_checkpoint(|| issue_past_proof(&storage, &input(), None))
                    .unwrap_err(),
                outbe_nod::errors::NodError::OfferAlreadyMade,
            );
        });
        assert_eq!(provider.storage, control.storage, "retry {write}");
    }
}

/// The offer of an owner for a Worldwide Day is spent with its Nod: once the Nod is mined
/// or forfeited, a second offer still reverts.
#[test]
fn a_second_offer_fails_after_the_first_nod_is_gone() {
    let _enclave = outbe_nod::test_support::scope();
    let mut provider = priced_world();
    StorageHandle::enter(&mut provider, |storage| {
        at_offer_hour(&storage, (1, 0), Some(100));
        let nod_id =
            issue_past_proof(&storage, &offer_of(CALLER, 840, ("100", "0")), None).unwrap();
        let item = nod_api::load_item(&storage, nod_id).unwrap().unwrap();
        let group = nod_api::load_group(&storage, item.body().group_key)
            .unwrap()
            .unwrap();
        nod_api::record_settled(&storage, item, group).unwrap();
        let item = nod_api::load_item(&storage, nod_id).unwrap().unwrap();
        let group = nod_api::load_group(&storage, item.body().group_key)
            .unwrap()
            .unwrap();
        nod_api::remove_nod(&storage, item, group).unwrap();
        assert!(nod_api::get_item(&storage, nod_id).unwrap().is_none());
        for currency in [840, 978] {
            assert_reverts(
                storage
                    .with_checkpoint(|| {
                        issue_past_proof(&storage, &offer_of(CALLER, currency, ("100", "0")), None)
                    })
                    .unwrap_err(),
                outbe_nod::errors::NodError::OfferAlreadyMade,
            );
        }
    });
}

/// The Nod goes to the sender the proof is bound to: an offer that names another account as
/// its creator reverts and takes nothing from that account.
#[test]
fn an_offer_cannot_issue_a_nod_to_another_account() {
    let _enclave = outbe_nod::test_support::scope();
    let victim = Address::repeat_byte(0x99);
    let mut provider = priced_world();
    StorageHandle::enter(&mut provider, |storage| {
        at_offer_hour(&storage, (1, 0), Some(100));
    });
    let before = provider.storage.clone();
    let events = provider.get_ordered_events().to_vec();
    StorageHandle::enter(&mut provider, |storage| {
        let (cipher_text, nonce, ephemeral_pubkey) =
            sealed_offer(victim, ("100", "0"), &[B256::repeat_byte(0x31)]);
        let gift = IssueNodInput {
            cipher_text,
            nonce,
            ephemeral_pubkey,
            ..offer(&[], &[])
        };
        let error = storage
            .with_checkpoint(|| issue_past_proof(&storage, &gift, None))
            .unwrap_err();
        let PrecompileError::RevertBytes(bytes) = error else {
            panic!("expected the enclave refusal, got {error:?}");
        };
        let refusal = INodFactory::EnclaveRejected::abi_decode(&bytes).unwrap();
        assert!(
            refusal.reason.contains("creator must be the sender"),
            "{}",
            refusal.reason
        );
    });
    assert_eq!(provider.storage, before, "a gift offer wrote state");
    assert_eq!(provider.get_ordered_events(), events);
    StorageHandle::enter(&mut provider, |storage| {
        issue_past_proof(&storage, &offer_of(victim, 840, ("100", "0")), None).unwrap();
    });
}
