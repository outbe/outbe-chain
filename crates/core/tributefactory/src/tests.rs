use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_agentreward::AgentRewardContract;
use outbe_oracle::{
    genesis::{init_from_genesis, OracleGenesisConfig},
    schema::OracleContract,
};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{
    metadosis_process_ready_binding, MetadosisCycleLifecycle, MetadosisMutationPurposeTag,
    StorageHandle,
};
use outbe_primitives::time::date_key_to_utc_timestamp;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::TributeContract;

use crate::runtime::{validate_agent_reward_addresses, OfferTributeInput};
use crate::schema::TributeFactoryContract;
use crate::worldwideday::genesis::{FreshDevnetGenesisBuilder, GenesisWorldwideDay};

const CHAIN_ID: u64 = outbe_primitives::chain::DEVNET_CHAIN_ID;

mod l2_zk_gate {
    use alloy_primitives::{Address, Bytes, U256};
    use outbe_l2registry::L2RegistryContract;
    use outbe_primitives::error::PrecompileError;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;
    use outbe_zk::demo_tribute::COMBINED_LEN as DEMO_TRIBUTE_COMBINED_LEN;

    use crate::runtime::OfferTributeInput;
    use crate::schema::TributeFactoryContract;

    const L2_CHAIN_ID: u64 = 0xdead;

    fn caller() -> Address {
        Address::repeat_byte(0x77)
    }

    /// A valid calendar day. Whether it is OFFERING depends on Metadosis state,
    /// which these fixtures leave empty.
    pub(super) const DAY: u32 = 20250115;

    fn offer(zk_merkle_root: &[u8], signature: &[u8]) -> OfferTributeInput {
        OfferTributeInput {
            caller: caller(),
            cipher_text: Bytes::new(),
            nonce: Bytes::new(),
            ephemeral_pubkey: U256::ZERO,
            worldwide_day: DAY.into(),
            tribute_currency: 840,
            reference_currency: 840,
            zk_proof: Bytes::new(),
            l2_chain_id: u32::try_from(L2_CHAIN_ID).unwrap(),
            circuit_version: "1.1.0".to_owned(),
            zk_merkle_root: Bytes::copy_from_slice(zk_merkle_root),
            signature: Bytes::copy_from_slice(signature),
        }
    }

    fn dummy_demo_tribute_proof(root: [u8; 32]) -> Bytes {
        let mut proof = Vec::with_capacity(DEMO_TRIBUTE_COMBINED_LEN);
        proof.extend_from_slice(&4u32.to_be_bytes());
        proof.extend_from_slice(&[0x01; 32]);
        proof.extend_from_slice(&[0x02; 32]);
        proof.extend_from_slice(&[0x03; 32]);
        proof.extend_from_slice(&root);
        proof.resize(DEMO_TRIBUTE_COMBINED_LEN, 0);
        proof.into()
    }

    // A signed, well-framed proof envelope lets these tests reach business
    // validation. Its dummy proof must never reach the crypto backend.
    fn signed_gate_offer(storage: StorageHandle<'_>) -> OfferTributeInput {
        use commonware_codec::Encode;
        use commonware_cryptography::bls12381::primitives::{
            ops, ops::sign_message, variant::MinSig,
        };
        let mut rng =
            <rand_commonware::rngs::StdRng as rand_commonware::SeedableRng>::from_seed([0x5a; 32]);
        let (private, public) = ops::keypair::<_, MinSig>(&mut rng);
        L2RegistryContract::new(storage)
            .register_network(
                L2_CHAIN_ID,
                caller(),
                &outbe_l2registry::public_key::encode(&public).unwrap(),
            )
            .unwrap();
        let root = [0x04; 32];
        let signature = sign_message::<MinSig>(
            &private,
            outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE,
            &root,
        )
        .encode();
        let mut input = offer(&root, &signature);
        input.zk_proof = dummy_demo_tribute_proof(root);
        input
    }

    #[test]
    fn offer_rejects_invalid_l2_signature() {
        use commonware_codec::Encode;
        use commonware_cryptography::bls12381::primitives::{
            ops::{self, sign_message},
            variant::MinSig,
        };

        let (private, public) = ops::keypair::<_, MinSig>(&mut rand_core_commonware::UnwrapErr(
            rand_commonware::rngs::SysRng,
        ));
        let public = outbe_l2registry::public_key::encode(&public).unwrap();
        let root = [0x04; 32];

        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let mut registry = L2RegistryContract::new(storage.clone());
            registry
                .register_network(L2_CHAIN_ID, caller(), &public)
                .unwrap();

            let mut factory = TributeFactoryContract::new(storage.clone());

            // The factory rejects a missing signature before oracle/metadosis/enclave work.
            let err = factory.offer_tribute(offer(&root, &[])).unwrap_err();
            assert_eq!(
                err.to_string(),
                PrecompileError::from(
                    outbe_l2registry::errors::L2RegistryError::InvalidZkSignature
                )
                .to_string()
            );

            // A valid signature passes this gate (no OFFERING day in this fixture).
            let good_sig = sign_message::<MinSig>(
                &private,
                outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE,
                &root,
            )
            .encode()
            .to_vec();
            // A signed root does not select a missing version or implicitly
            // adopt the global registry's newer Demo Tribute version.
            for version in ["", "1.2.0"] {
                let mut wrong_version = offer(&root, &good_sig);
                wrong_version.zk_proof = dummy_demo_tribute_proof(root);
                wrong_version.circuit_version = version.to_owned();
                let error = factory.offer_tribute(wrong_version).unwrap_err();
                let expected = crate::errors::TributeFactoryError::UnknownCircuitVersion {
                    chain_id: u32::try_from(L2_CHAIN_ID).unwrap(),
                    version: version.to_owned(),
                };
                assert_eq!(
                    error.to_string(),
                    PrecompileError::from(expected).to_string()
                );
            }
            let mut valid_gate = offer(&root, &good_sig);
            // The submitter need not be the registered network operator.
            valid_gate.caller = Address::repeat_byte(0x88);
            valid_gate.zk_proof = dummy_demo_tribute_proof(root);
            let mut factory = TributeFactoryContract::new(storage.clone());
            let err = factory.offer_tribute(valid_gate).unwrap_err();
            assert_eq!(
                err.to_string(),
                PrecompileError::from(
                    crate::errors::TributeFactoryError::WorldwideDayNotOffering {
                        worldwide_day: DAY.into(),
                    }
                )
                .to_string()
            );

            // A caller's registration on another chain cannot authorize an
            // offer for a chain that is no longer registered.
            registry.remove_network(caller(), L2_CHAIN_ID).unwrap();
            registry.register_network(4242, caller(), &public).unwrap();
            let mut wrong_chain = offer(&root, &good_sig);
            wrong_chain.zk_proof = dummy_demo_tribute_proof(root);
            let error = factory.offer_tribute(wrong_chain).unwrap_err();
            let expected = outbe_l2registry::errors::L2RegistryError::NetworkNotRegistered {
                chain_id: L2_CHAIN_ID,
            };
            assert_eq!(
                error.to_string(),
                PrecompileError::from(expected).to_string()
            );
        });
    }

    #[test]
    fn registered_network_requires_proof_and_matching_public_root() {
        use commonware_codec::Encode;
        use commonware_cryptography::bls12381::primitives::{
            ops::{self, sign_message},
            variant::MinSig,
        };

        let (private, public) = ops::keypair::<_, MinSig>(&mut rand_core_commonware::UnwrapErr(
            rand_commonware::rngs::SysRng,
        ));
        let public = outbe_l2registry::public_key::encode(&public).unwrap();
        let root = [0x04; 32];
        let signature = sign_message::<MinSig>(
            &private,
            outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE,
            &root,
        )
        .encode()
        .to_vec();

        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let mut registry = L2RegistryContract::new(storage.clone());
            registry
                .register_network(L2_CHAIN_ID, caller(), &public)
                .unwrap();

            let mut factory = TributeFactoryContract::new(storage.clone());
            let missing = factory.offer_tribute(offer(&root, &signature)).unwrap_err();
            assert_eq!(
                missing.to_string(),
                PrecompileError::from(crate::errors::TributeFactoryError::ZkProofRequired)
                    .to_string()
            );

            let mut wrong_root = offer(&root, &signature);
            wrong_root.zk_proof = dummy_demo_tribute_proof([0x24; 32]);
            let mut factory = TributeFactoryContract::new(storage.clone());
            let mismatch = factory.offer_tribute(wrong_root).unwrap_err();
            assert_eq!(
                mismatch.to_string(),
                PrecompileError::from(crate::errors::TributeFactoryError::ZkPublicInputMismatch {
                    field: "merkle_root",
                })
                .to_string()
            );
        });
    }

    #[test]
    fn unregistered_chains_cannot_offer() {
        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            // An unregistered chain must fail before any enclave work.
            let mut factory = TributeFactoryContract::new(storage.clone());
            let err = factory
                .offer_tribute_with_processor(offer(&[], &[]), |_| {
                    panic!("unregistered chain reached the enclave")
                })
                .unwrap_err();
            let expected = outbe_l2registry::errors::L2RegistryError::NetworkNotRegistered {
                chain_id: L2_CHAIN_ID,
            };
            assert_eq!(err.to_string(), PrecompileError::from(expected).to_string());
        });
    }

    /// `worldwideDay` and `tributeCurrency` are cleartext ABI arguments precisely
    /// so a bad one costs no enclave round trip. These fixtures configure no
    /// enclave client at all, so reaching the sidecar would surface as
    /// `tee_sidecar_unavailable`. The assertions below prove that the host
    /// rejected first.
    #[test]
    fn host_rejects_an_invalid_calendar_day_before_the_enclave() {
        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let mut bad_day = signed_gate_offer(storage.clone());
            bad_day.worldwide_day = 20250230u32.into(); // February 30th

            let mut factory = TributeFactoryContract::new(storage);
            let err = factory.offer_tribute(bad_day).unwrap_err();
            assert_eq!(
                err.to_string(),
                PrecompileError::from(crate::errors::TributeFactoryError::InvalidWorldwideDay {
                    worldwide_day: 20250230u32.into(),
                })
                .to_string()
            );
        });
    }

    /// The day check runs before the currency check, so this case needs the day to
    /// be OFFERING first. These fixtures cannot arrange that. Assert the ordering
    /// instead: an unregistered currency paired with a non-OFFERING day still
    /// reports the day. This proves that the currency lookup is not reached, and
    /// therefore that neither reaches the enclave.
    #[test]
    fn host_rejects_a_non_offering_day_before_pricing() {
        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let mut unpriced = signed_gate_offer(storage.clone());
            unpriced.tribute_currency = 999; // never registered

            let mut factory = TributeFactoryContract::new(storage);
            let err = factory.offer_tribute(unpriced).unwrap_err();
            assert_eq!(
                err.to_string(),
                PrecompileError::from(
                    crate::errors::TributeFactoryError::WorldwideDayNotOffering {
                        worldwide_day: DAY.into(),
                    }
                )
                .to_string()
            );
        });
    }
}

#[test]
fn test_validate_agent_reward_both_empty() {
    assert!(validate_agent_reward_addresses(&[], &[]).is_ok());
}

#[test]
fn test_validate_agent_reward_both_present() {
    let wallets = vec!["0x1111111111111111111111111111111111111111".to_string()];
    let sfas = vec!["0x2222222222222222222222222222222222222222".to_string()];
    assert!(validate_agent_reward_addresses(&wallets, &sfas).is_ok());
}

#[test]
fn test_validate_agent_reward_wallets_only() {
    let wallets = vec!["0x1111111111111111111111111111111111111111".to_string()];
    assert!(validate_agent_reward_addresses(&wallets, &[]).is_err());
}

#[test]
fn test_validate_agent_reward_sfa_only() {
    let sfas = vec!["0x2222222222222222222222222222222222222222".to_string()];
    assert!(validate_agent_reward_addresses(&[], &sfas).is_err());
}

#[test]
fn test_validate_agent_reward_invalid_address() {
    let wallets = vec!["not_a_valid_address".to_string()];
    let sfas = vec!["0x2222222222222222222222222222222222222222".to_string()];
    assert!(validate_agent_reward_addresses(&wallets, &sfas).is_err());
}

const TARGET_WWD_A: WorldwideDay = WorldwideDay::new(20_260_802);
const REWARD_WALLET: Address = Address::repeat_byte(0x71);
const REWARD_SRA: Address = Address::repeat_byte(0x72);

fn seed_offer_world(storage: StorageHandle<'_>, target_days: &[WorldwideDay]) {
    let now = u64::try_from(storage.timestamp().unwrap()).unwrap();
    let mut metadosis = FreshDevnetGenesisBuilder::new();
    for (index, worldwide_day) in target_days.iter().copied().enumerate() {
        let offset = u64::try_from(index).unwrap() * 10;
        metadosis = metadosis.seed_active_worldwide_day(GenesisWorldwideDay {
            worldwide_day,
            day: 0,
            forming_start: offset + 1,
            forming_end: offset + 2,
            offerings_start: offset + 3,
            offering_end: now.max(offset + 3) + 86_400,
        });
    }
    metadosis.apply(storage.clone()).unwrap();

    let mut oracle = OracleContract::new(storage.clone());
    init_from_genesis(&mut oracle, &OracleGenesisConfig::default_config()).unwrap();
    let pair = AddressPair::new_coen_to(840);
    for worldwide_day in target_days {
        let start = worldwide_day.start_timestamp();
        oracle
            .write_snapshot(start + 1, &[(pair, U256::from(100), U256::ONE)])
            .unwrap();
        oracle
            .store_worldwide_day_vwap_snapshot(*worldwide_day, start, start + 50 * 60 * 60)
            .unwrap();
    }
}

#[test]
#[ignore = "requires the pinned Barretenberg CRS"]
fn real_zk_offer_records_rewards_once_and_rolls_back_failures() {
    let proof = include_bytes!(
        "../../../../testing/protocol-benchmarks/fixtures/tribute_demo_tribute_v1.bin"
    );
    assert_real_zk_offer(proof, 0xdead, "1.1.0");
}

#[test]
#[ignore = "requires the pinned Barretenberg CRS"]
fn inbox_key_real_zk_offer_records_rewards_once_and_rolls_back_failures() {
    let proof = include_bytes!(
        "../../../../testing/protocol-benchmarks/fixtures/tribute_demo_tribute_v1.bin"
    );
    assert_real_zk_offer_with_key_source(proof, 0xdead, "1.1.0", true);
}

#[test]
#[ignore = "generates a real Barretenberg proof with the pinned CRS"]
fn niflheim_tribute_chain_9900501_issues_and_rejects_replay() {
    use ark_bn254::Fr;
    use ark_ff::{BigInteger, PrimeField};
    use outbe_zk::claim::TributeDraftClaim;
    use outbe_zk::noir::{niflheim_tribute, EmbeddedCurvePoint};
    use outbe_zk::primitive::curve::coords;
    use outbe_zk::primitive::hash::{self, poseidon2};
    use outbe_zk::protocol::{
        entity::Entity as EntityTrait,
        key::{NftSigner, Signer},
        zk::ProofGenerator,
    };
    use outbe_zk_backend::barretenberg::{Barretenberg, BarretenbergHandler};
    use rand::{rngs::StdRng, SeedableRng};

    const L2_CHAIN_ID: u32 = 9_900_501;
    let mut rng = StdRng::from_seed([0x99; 32]);
    let signer = Signer::local(&mut rng).unwrap();
    let seed = signer.owner_seed();
    let owner = seed.derive_owner().unwrap();
    let draft = TributeDraftClaim {
        id: B256::with_last_byte(0x11),
        derived_owner: B256::from_slice(&owner.into_bigint().to_bytes_be()),
        worldwide_day: TARGET_WWD_A.into(),
        currency: 840,
        base: 100,
        atto: 0,
        su_ids: vec![B256::with_last_byte(0x22)],
    };
    let nft_hash = draft.entity_hash().unwrap();
    let commitment_id: [u8; 32] = draft.id.into();
    let binding_hash = hash::binding(
        &Address::repeat_byte(0x77).into_array(),
        &commitment_id,
        CHAIN_ID,
        u64::from(L2_CHAIN_ID),
    )
    .unwrap();
    let message = hash::signing_payload(nft_hash, seed.nonce, binding_hash).unwrap();
    let signature = signer.sign(&mut rng, message).unwrap();
    let (x, y) = coords(&seed.pk).unwrap();
    // Niflheim uses two-input Poseidon2 without Demo's domain separator.
    // Insert the draft at the leftmost leaf of a depth-32 empty tree.
    let mut merkle_root = nft_hash;
    let mut sibling = Fr::from(0u64);
    let mut siblings = [sibling; 32];
    for level in &mut siblings {
        *level = sibling;
        merkle_root = poseidon2(&[merkle_root, sibling]).unwrap();
        sibling = poseidon2(&[sibling, sibling]).unwrap();
    }
    let witness = niflheim_tribute::Witness {
        pk: EmbeddedCurvePoint { x, y },
        signature,
        nonce: seed.nonce,
        merkle_path_siblings: siblings,
        merkle_path_indices: [1; 32],
    };
    let public = niflheim_tribute::PublicInputs {
        owner,
        nft_hash,
        binding_hash,
        merkle_root,
    };
    BarretenbergHandler::init().unwrap();
    let proof = ProofGenerator::<niflheim_tribute::NiflheimTribute>::generate(
        &Barretenberg::default(),
        &witness,
        &public,
    )
    .unwrap();
    let combined = niflheim_tribute::encode_combined_proof(public, proof.proof).unwrap();
    assert_real_zk_offer(&combined, L2_CHAIN_ID, "1.0.0");
}

fn assert_real_zk_offer(proof: &[u8], l2_chain_id: u32, circuit_version: &str) {
    assert_real_zk_offer_with_key_source(proof, l2_chain_id, circuit_version, false);
}

fn assert_real_zk_offer_with_key_source(
    proof: &[u8],
    l2_chain_id: u32,
    circuit_version: &str,
    from_inbox: bool,
) {
    use alloy_sol_types::{sol, SolCall, SolError, SolValue};
    use commonware_codec::Encode;
    use commonware_cryptography::bls12381::primitives::{
        ops::{self, sign_message},
        variant::MinSig,
    };
    use outbe_l2registry::L2RegistryContract;
    use outbe_tee_enclave::process::{process_tribute_offer_batch, TributeOfferKeyMaterial};
    use outbe_zk::wire::decode_public_words;
    use x25519_dalek::{PublicKey, StaticSecret};

    const CALLER: Address = Address::repeat_byte(0x77);
    const REWARD_DAY: u32 = 20_260_803;
    const PRIVATE_KEY: [u8; 32] = [0x33; 32];
    let _enclave = outbe_tribute::enclave_client::test_enclave::scope();
    outbe_zk_backend::barretenberg::BarretenbergHandler::init().unwrap();
    let merkle_root = B256::from(decode_public_words::<4>(proof, proof.len()).unwrap()[3]);
    let payload = serde_json::to_vec(&serde_json::json!({
        "creator": format!("{CALLER:#x}"),
        "tribute_draft_id": format!("{:#x}", B256::with_last_byte(0x11)),
        "amount_base": "100",
        "amount_micro": "0",
        "su_hashes": [format!("{:#x}", B256::with_last_byte(0x22))],
        "wallet_addresses": [format!("{REWARD_WALLET:#x}")],
        "sra_addresses": [format!("{REWARD_SRA:#x}")],
    }))
    .unwrap();
    let offer_public_key = PublicKey::from(&StaticSecret::from(PRIVATE_KEY)).to_bytes();
    let (cipher, nonce, ephemeral) =
        outbe_tee::offer_encrypt::encrypt_tribute_offer(&offer_public_key, &payload).unwrap();
    let key = TributeOfferKeyMaterial {
        tribute_offer_private_key: &PRIVATE_KEY,
        salt: &outbe_tee::OFFER_HKDF_SALT,
    };
    let mut rng =
        <rand_commonware::rngs::StdRng as rand_commonware::SeedableRng>::from_seed([0x5a; 32]);
    let (private, group_key) = ops::keypair::<_, MinSig>(&mut rng);
    let signature = sign_message::<MinSig>(
        &private,
        outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE,
        merkle_root.as_slice(),
    )
    .encode();
    let make_offer = || OfferTributeInput {
        caller: CALLER,
        cipher_text: Bytes::copy_from_slice(&cipher),
        nonce: Bytes::copy_from_slice(&nonce),
        ephemeral_pubkey: U256::from_be_bytes(ephemeral),
        worldwide_day: TARGET_WWD_A,
        tribute_currency: 840,
        reference_currency: 840,
        zk_proof: Bytes::copy_from_slice(proof),
        l2_chain_id,
        circuit_version: circuit_version.to_owned(),
        zk_merkle_root: Bytes::copy_from_slice(merkle_root.as_slice()),
        signature: Bytes::copy_from_slice(&signature),
    };
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let network_key = outbe_l2registry::public_key::encode(&group_key).unwrap();
    let inbox = Address::repeat_byte(0x88);
    if from_inbox {
        sol! {
            function groupPubKey() external view returns (bytes memory);
        }
        provider.stub_sub_call_at_selector(
            inbox,
            groupPubKeyCall::SELECTOR,
            (Bytes::copy_from_slice(&network_key),)
                .abi_encode_params()
                .into(),
        );
    }
    provider.set_timestamp(U256::from(date_key_to_utc_timestamp(REWARD_DAY) + 43_200));
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);

    StorageHandle::enter(&mut provider, |storage| {
        seed_offer_world(storage.clone(), &[TARGET_WWD_A]);
        let mut registry = L2RegistryContract::new(storage.clone());
        registry
            .register_network(
                u64::from(l2_chain_id),
                inbox,
                if from_inbox { &[] } else { &network_key },
            )
            .unwrap();
    });
    let before_offer = provider.storage.clone();
    provider.fail_after_mutation_at(usize::MAX);
    let tribute_id = StorageHandle::enter(&mut provider, |storage| {
        let mut factory = TributeFactoryContract::new(storage.clone());
        let id = factory
            .offer_tribute_with_processor(make_offer(), |offers| {
                Ok(process_tribute_offer_batch(&key, offers).0)
            })
            .unwrap();
        let stored = TributeContract::new(storage.clone())
            .get_tribute(id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.owner, CALLER);
        assert_eq!(stored.worldwide_day, TARGET_WWD_A);
        id
    });
    let mutations = provider.clear_mutation_failure();
    StorageHandle::enter(&mut provider, |storage| {
        let mut factory = TributeFactoryContract::new(storage.clone());
        let replay = factory
            .offer_tribute_with_processor(make_offer(), |offers| {
                Ok(process_tribute_offer_batch(&key, offers).0)
            })
            .unwrap_err();
        assert!(matches!(replay, PrecompileError::RevertBytes(_)));
        let rewards = AgentRewardContract::new(storage);
        assert_eq!(
            rewards.get_all_waa_counts(REWARD_DAY.into()).unwrap(),
            vec![(REWARD_WALLET, 1)]
        );
        assert_eq!(
            rewards.get_all_sra_counts(REWARD_DAY.into()).unwrap(),
            vec![(REWARD_SRA, 1)]
        );
        assert!(rewards.get_all_waa_counts(TARGET_WWD_A).unwrap().is_empty());
    });

    // Fail the final mutation of the real offer, after Tribute issuance and
    // reward writes start, inside the enclosing VM transaction checkpoint.
    provider.storage = before_offer.clone();
    provider.fail_after_mutation_at(mutations - 1);
    StorageHandle::enter(&mut provider, |storage| {
        storage
            .with_checkpoint(|| {
                TributeFactoryContract::new(storage.clone())
                    .offer_tribute_with_processor(make_offer(), |offers| {
                        Ok(process_tribute_offer_batch(&key, offers).0)
                    })
            })
            .unwrap_err();
    });
    assert_eq!(provider.clear_mutation_failure(), mutations);
    assert_eq!(
        provider.storage, before_offer,
        "failed offer leaked issued or reward state"
    );

    // Overflow SRA only after the real proof, SU marking, issuance and fresh WAA writes.
    // Reward activity belongs to the execution day, not TARGET_WWD_A.
    let reward_day = WorldwideDay::new(REWARD_DAY);
    let count_key = AgentRewardContract::tribute_count_key(reward_day, REWARD_SRA);
    let index_key = AgentRewardContract::address_index_key(reward_day, 0);
    StorageHandle::enter(&mut provider, |storage| {
        let rewards = AgentRewardContract::new(storage);
        rewards.sra_addresses.write(&index_key, REWARD_SRA).unwrap();
        rewards.sra_address_count.write(&reward_day, 1).unwrap();
        rewards
            .sra_tribute_counts
            .write(&count_key, u64::MAX)
            .unwrap();
        rewards
            .sra_total_count
            .write(&reward_day, u64::MAX)
            .unwrap();
    });
    let before_overflow = provider.storage.clone();
    let events_before_overflow = provider.events.clone();
    let ordered_events_before_overflow = provider.get_ordered_events().to_vec();
    StorageHandle::enter(&mut provider, |storage| {
        let error = storage
            .with_checkpoint(|| {
                TributeFactoryContract::new(storage.clone())
                    .offer_tribute_with_processor(make_offer(), |offers| {
                        Ok(process_tribute_offer_batch(&key, offers).0)
                    })
            })
            .unwrap_err();
        let PrecompileError::RevertBytes(bytes) = error else {
            panic!("expected counter-overflow revert bytes, got {error:?}");
        };
        assert_eq!(
            bytes.as_ref(),
            outbe_agentreward::precompile::IAgentReward::TributeCounterOverflow {}.abi_encode()
        );
        assert!(TributeContract::new(storage.clone())
            .get_record(tribute_id)
            .unwrap()
            .is_none());
        assert!(!TributeFactoryContract::new(storage.clone())
            .has_used_su_hash(B256::with_last_byte(0x22))
            .unwrap());
        let rewards = AgentRewardContract::new(storage);
        assert!(rewards.get_all_waa_counts(reward_day).unwrap().is_empty());
        assert_eq!(rewards.waa_address_count.read(&reward_day).unwrap(), 0);
        assert_eq!(rewards.waa_total_count.read(&reward_day).unwrap(), 0);
        assert_eq!(
            rewards
                .waa_tribute_counts
                .read(&AgentRewardContract::tribute_count_key(
                    reward_day,
                    REWARD_WALLET
                ))
                .unwrap(),
            0
        );
        assert_eq!(
            rewards
                .waa_addresses
                .read(&AgentRewardContract::address_index_key(reward_day, 0))
                .unwrap(),
            Address::ZERO
        );
        assert_eq!(
            rewards.get_all_sra_counts(reward_day).unwrap(),
            vec![(REWARD_SRA, u64::MAX)]
        );
        assert_eq!(rewards.sra_address_count.read(&reward_day).unwrap(), 1);
        assert_eq!(rewards.sra_total_count.read(&reward_day).unwrap(), u64::MAX);
        assert_eq!(rewards.sra_addresses.read(&index_key).unwrap(), REWARD_SRA);
        assert!(rewards.get_all_waa_counts(TARGET_WWD_A).unwrap().is_empty());
        assert!(rewards.get_all_sra_counts(TARGET_WWD_A).unwrap().is_empty());
    });
    assert_eq!(
        provider.storage, before_overflow,
        "SRA overflow leaked offer state"
    );
    assert_eq!(provider.events, events_before_overflow);
    assert_eq!(
        provider.get_ordered_events(),
        ordered_events_before_overflow
    );
}

#[test]
fn empty_su_hash_list_rejects_before_issuance_and_rewards() {
    use alloy_sol_types::SolError;
    use commonware_codec::Encode;
    use commonware_cryptography::bls12381::primitives::{
        ops::{self, sign_message},
        variant::MinSig,
    };
    use outbe_l2registry::L2RegistryContract;
    use outbe_tee::protocol::{TributeOfferResult, TributeOfferStatus};
    use outbe_zk::demo_tribute::COMBINED_LEN as DEMO_TRIBUTE_COMBINED_LEN;

    const CALLER: Address = Address::repeat_byte(0x77);
    const L2_CHAIN_ID: u64 = 0xdead;
    let root = [0x04; 32];
    let mut rng =
        <rand_commonware::rngs::StdRng as rand_commonware::SeedableRng>::from_seed([0x5a; 32]);
    let (private, public) = ops::keypair::<_, MinSig>(&mut rng);
    let network_key = outbe_l2registry::public_key::encode(&public).unwrap();
    let signature = sign_message::<MinSig>(
        &private,
        outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE,
        &root,
    )
    .encode();
    let mut proof = Vec::with_capacity(DEMO_TRIBUTE_COMBINED_LEN);
    proof.extend_from_slice(&4u32.to_be_bytes());
    proof.extend_from_slice(&[0x01; 32]);
    proof.extend_from_slice(&[0x02; 32]);
    proof.extend_from_slice(&[0x03; 32]);
    proof.extend_from_slice(&root);
    proof.resize(DEMO_TRIBUTE_COMBINED_LEN, 0);

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(date_key_to_utc_timestamp(20_260_803) + 43_200));
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);
    StorageHandle::enter(&mut provider, |storage| {
        seed_offer_world(storage.clone(), &[TARGET_WWD_A]);
        L2RegistryContract::new(storage)
            .register_network(L2_CHAIN_ID, CALLER, &network_key)
            .unwrap();
    });
    let before = provider.storage.clone();
    let events_before = provider.events.clone();
    let ordered_before = provider.get_ordered_events().to_vec();

    let input = OfferTributeInput {
        caller: CALLER,
        cipher_text: Bytes::from(vec![1, 2, 3]),
        nonce: Bytes::from(vec![4; 12]),
        ephemeral_pubkey: U256::from(1u64),
        worldwide_day: TARGET_WWD_A,
        tribute_currency: 840,
        reference_currency: 840,
        zk_proof: Bytes::from(proof),
        l2_chain_id: u32::try_from(L2_CHAIN_ID).unwrap(),
        circuit_version: "1.1.0".to_owned(),
        zk_merkle_root: Bytes::copy_from_slice(&root),
        signature: Bytes::from(signature),
    };

    StorageHandle::enter(&mut provider, |storage| {
        let error = storage
            .with_checkpoint(|| {
                TributeFactoryContract::new(storage.clone()).offer_tribute_with_processor(
                    input,
                    |_| {
                        Ok(vec![TributeOfferResult {
                            token_id: B256::repeat_byte(0x11),
                            owner: CALLER,
                            issuance_amount_minor: U256::from(1u64),
                            nominal_amount_minor: U256::from(1u64),
                            effective_reference_price_minor: U256::from(1u64),
                            su_hashes: Vec::new(),
                            wallet_addresses: vec![format!("{REWARD_WALLET:#x}")],
                            sra_addresses: vec![format!("{REWARD_SRA:#x}")],
                            zk_expected_hashes: None,
                            status: TributeOfferStatus::Created,
                        }])
                    },
                )
            })
            .unwrap_err();
        let PrecompileError::RevertBytes(bytes) = error else {
            panic!("expected empty-SU revert bytes, got {error:?}");
        };
        assert_eq!(
            bytes.as_ref(),
            crate::precompile::ITributeFactory::EmptySuHashList {}.abi_encode()
        );
        let tribute = TributeContract::new(storage.clone());
        assert_eq!(tribute.balance_of(CALLER).unwrap(), 0);
        assert!(!TributeFactoryContract::new(storage.clone())
            .has_used_su_hash(B256::repeat_byte(0x22))
            .unwrap());
        let rewards = AgentRewardContract::new(storage);
        let reward_day = WorldwideDay::new(20_260_803);
        assert!(rewards.get_all_waa_counts(reward_day).unwrap().is_empty());
        assert!(rewards.get_all_sra_counts(reward_day).unwrap().is_empty());
    });
    assert_eq!(provider.storage, before, "empty SU admission wrote state");
    assert_eq!(provider.events, events_before);
    assert_eq!(provider.get_ordered_events(), ordered_before);
}

#[test]
fn su_hash_can_only_be_used_once() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let events_before = storage.events.clone();
    let ordered_before = storage.get_ordered_events().to_vec();
    StorageHandle::enter(&mut storage, |storage| {
        let expect_reused = |err: PrecompileError, _expected_hash: B256| {
            assert_eq!(
                err.to_string(),
                PrecompileError::from(crate::errors::TributeFactoryError::SuHashAlreadyUsed)
                    .to_string()
            );
        };

        // Distinct hashes across the array all succeed.
        let (a, b) = (B256::repeat_byte(0xAA), B256::repeat_byte(0xBB));
        TributeFactoryContract::new(storage.clone())
            .mark_su_hashes_used(&[a, b])
            .expect("distinct hashes ok");

        // The factory rejects reuse in a later call (persistent marker).
        expect_reused(
            TributeFactoryContract::new(storage.clone())
                .mark_su_hashes_used(&[a])
                .unwrap_err(),
            a,
        );

        // The factory rejects a duplicate within a single array.
        let dup = B256::repeat_byte(0xCC);
        expect_reused(
            TributeFactoryContract::new(storage.clone())
                .mark_su_hashes_used(&[dup, dup])
                .unwrap_err(),
            dup,
        );
    });
    assert_eq!(storage.events, events_before);
    assert_eq!(storage.get_ordered_events(), ordered_before);
}

#[test]
fn test_storage_dsl_layout_is_compatible_with_previous_slots() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let factory = TributeFactoryContract::new(storage.clone());
        assert_eq!(
            factory.used_su_hashes.base_slot(),
            alloy_primitives::U256::ZERO
        );
    });
}

#[test]
fn currency_price_unavailable_encodes_selector_and_pipeline_cap_and_historical_close() {
    use alloy_sol_types::SolError;
    let wwd = WorldwideDay::new(20261001);
    let err: PrecompileError =
        crate::errors::TributeFactoryError::CurrencyPriceUnavailable { worldwide_day: wwd }.into();
    let expected_bytes: Bytes = crate::precompile::ITributeFactory::CurrencyPriceUnavailable {
        worldwideDay: wwd.value(),
    }
    .abi_encode()
    .into();
    assert!(matches!(err, PrecompileError::RevertBytes(bytes) if bytes == expected_bytes));

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 30);
    StorageHandle::enter(&mut provider, |storage| {
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                let mut factory = TributeFactoryContract::new(storage.clone());
                let mut day_key = 20261001u32;
                for _ in 0..crate::worldwideday::constants::MAX_PIPELINE_WWDS {
                    let cur_wwd = WorldwideDay::new(day_key);
                    let (schedule, _) = crate::worldwideday::compute_wwd_schedule(cur_wwd)?;
                    assert!(crate::worldwideday::create_worldwide_day(
                        &mut factory,
                        cur_wwd,
                        schedule
                    )?);
                    day_key = outbe_primitives::time::next_date_key(day_key).unwrap();
                }
                let overflow_wwd = WorldwideDay::new(day_key);
                let (overflow_schedule, _) =
                    crate::worldwideday::compute_wwd_schedule(overflow_wwd)?;
                let cap_err = crate::worldwideday::create_worldwide_day(
                    &mut factory,
                    overflow_wwd,
                    overflow_schedule,
                )
                .unwrap_err();
                assert!(cap_err.to_string().contains("24"));
                Ok(())
            })
            .unwrap();

        // Historical catch-up (offering_end <= now) creates WWD in Closed state without touching pipeline_wwd.
        let hist_wwd = WorldwideDay::new(20260901);
        let (hist_schedule, _) = crate::worldwideday::compute_wwd_schedule(hist_wwd).unwrap();
        storage
            .set_block_timestamp(U256::from(hist_schedule.offering_end))
            .unwrap();
        let binding2 = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding2, || {
                let mut factory = TributeFactoryContract::new(storage.clone());
                assert!(crate::worldwideday::create_worldwide_day(
                    &mut factory,
                    hist_wwd,
                    hist_schedule
                )?);
                let rec = factory.worldwide_days.get(hist_wwd)?.unwrap();
                assert_eq!(rec.state, crate::worldwideday::wwd_state::CLOSED);
                assert!(!factory.pipeline_wwd.read_all()?.contains(&hist_wwd));
                assert!(!factory.active_wwd.read_all()?.contains(&hist_wwd));
                assert!(TributeContract::new(storage.clone()).is_day_sealed(hist_wwd)?);
                Ok(())
            })
            .unwrap();
    });
}
