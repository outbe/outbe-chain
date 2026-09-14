use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_agentreward::AgentRewardContract;
use outbe_compressed_entities::{
    begin_block, EntityRef, ExecutionScope, IdPage, IdPageRequest, ParentBodySource,
    ParentBodySourceError, QueryRef, StoredBody,
};
use outbe_metadosis::{
    genesis::{FreshDevnetGenesisBuilder, GenesisWorldwideDay},
    WwdDayType, WwdStatus,
};
use outbe_oracle::{
    genesis::{init_from_genesis, OracleGenesisConfig},
    schema::OracleContract,
};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::date_key_to_utc_timestamp;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::TributeContract;

use crate::runtime::{validate_agent_reward_addresses, OfferTributeInput};
use crate::schema::TributeFactoryContract;

const CHAIN_ID: u64 = outbe_primitives::chain::DEVNET_CHAIN_ID;

struct NoParentBodies;

impl ParentBodySource for NoParentBodies {
    fn get(
        &self,
        _entity: EntityRef,
    ) -> core::result::Result<Option<StoredBody>, ParentBodySourceError> {
        Ok(None)
    }

    fn list(
        &self,
        _query: QueryRef,
        _request: IdPageRequest,
    ) -> core::result::Result<IdPage, ParentBodySourceError> {
        Ok(IdPage {
            ids: Vec::new(),
            next_after: None,
        })
    }
}

mod l2_zk_gate {
    use alloy_primitives::{Address, Bytes, U256};
    use outbe_compressed_entities::ExecutionScope;
    use outbe_l2registry::L2RegistryContract;
    use outbe_primitives::error::PrecompileError;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;
    use outbe_zk_canonical::full_proof::COMBINED_LEN as FULL_PROOF_COMBINED_LEN;

    use super::NoParentBodies;
    use crate::runtime::OfferTributeInput;
    use crate::schema::TributeFactoryContract;

    const L2_CHAIN_ID: u64 = 0xdead;

    fn caller() -> Address {
        Address::repeat_byte(0x77)
    }

    /// A valid calendar day; whether it is OFFERING depends on Metadosis state,
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
            exclude_from_intex_issuance: false,
            zk_proof: Bytes::new(),
            l2_chain_id: u32::try_from(L2_CHAIN_ID).unwrap(),
            circuit_version: "1.1.0".to_owned(),
            zk_merkle_root: Bytes::copy_from_slice(zk_merkle_root),
            signature: Bytes::copy_from_slice(signature),
        }
    }

    fn dummy_full_proof(root: [u8; 32]) -> Bytes {
        let mut proof = Vec::with_capacity(FULL_PROOF_COMBINED_LEN);
        proof.extend_from_slice(&4u32.to_be_bytes());
        proof.extend_from_slice(&[0x01; 32]);
        proof.extend_from_slice(&[0x02; 32]);
        proof.extend_from_slice(&[0x03; 32]);
        proof.extend_from_slice(&root);
        proof.resize(FULL_PROOF_COMBINED_LEN, 0);
        proof.into()
    }

    fn revert_message(err: PrecompileError) -> String {
        match err {
            PrecompileError::Revert(msg) => msg,
            other => panic!("expected revert, got {other:?}"),
        }
    }

    // A signed, well-framed proof envelope lets these tests reach business
    // validation; its dummy proof must never reach the crypto backend.
    fn signed_gate_offer(storage: StorageHandle<'_>) -> OfferTributeInput {
        use commonware_codec::Encode;
        use commonware_cryptography::bls12381::primitives::{
            ops, ops::sign_message, variant::MinSig,
        };
        let mut rng =
            <rand_commonware::rngs::StdRng as rand_commonware::SeedableRng>::from_seed([0x5a; 32]);
        let (private, public) = ops::keypair::<_, MinSig>(&mut rng);
        L2RegistryContract::new(storage)
            .register_network(L2_CHAIN_ID, caller(), &public.encode())
            .unwrap();
        let root = [0x04; 32];
        let signature = sign_message::<MinSig>(
            &private,
            outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE,
            &root,
        )
        .encode();
        let mut input = offer(&root, &signature);
        input.zk_proof = dummy_full_proof(root);
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
        let public = public.encode().to_vec();
        let root = [0x04; 32];

        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let mut registry = L2RegistryContract::new(storage.clone());
            registry
                .register_network(L2_CHAIN_ID, caller(), &public)
                .unwrap();

            let scope = ExecutionScope::new();
            let mut factory = TributeFactoryContract::new(storage.clone());

            // A missing signature is rejected before oracle/metadosis/enclave work.
            let err = factory
                .offer_tribute(&scope, &NoParentBodies, offer(&root, &[]))
                .unwrap_err();
            assert!(revert_message(err).contains("invalid BLS signature"));

            // A valid signature passes this gate (no OFFERING day in this fixture).
            let good_sig = sign_message::<MinSig>(
                &private,
                outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE,
                &root,
            )
            .encode()
            .to_vec();
            // A signed root does not select a missing version or implicitly
            // adopt the global registry's newer FullProof version.
            for version in ["", "1.2.0"] {
                let mut wrong_version = offer(&root, &good_sig);
                wrong_version.zk_proof = dummy_full_proof(root);
                wrong_version.circuit_version = version.to_owned();
                let error = factory
                    .offer_tribute(&scope, &NoParentBodies, wrong_version)
                    .unwrap_err();
                let expected = crate::errors::TributeFactoryError::UnknownCircuitVersion {
                    chain_id: u32::try_from(L2_CHAIN_ID).unwrap(),
                    version: version.to_owned(),
                };
                assert_eq!(revert_message(error), expected.to_string());
            }
            let mut valid_gate = offer(&root, &good_sig);
            valid_gate.zk_proof = dummy_full_proof(root);
            let mut factory = TributeFactoryContract::new(storage.clone());
            let err = factory
                .offer_tribute(&scope, &NoParentBodies, valid_gate)
                .unwrap_err();
            assert!(revert_message(err).contains("is not in OFFERING status"));

            // The otherwise valid selector cannot be borrowed by an operator
            // registered on another L2, even with that L2's valid signature.
            registry.remove_network(caller(), L2_CHAIN_ID).unwrap();
            registry.register_network(4242, caller(), &public).unwrap();
            let mut wrong_chain = offer(&root, &good_sig);
            wrong_chain.zk_proof = dummy_full_proof(root);
            let error = factory
                .offer_tribute(&scope, &NoParentBodies, wrong_chain)
                .unwrap_err();
            let expected = crate::errors::TributeFactoryError::CircuitChainMismatch {
                provided: u32::try_from(L2_CHAIN_ID).unwrap(),
                registered: 4242,
            };
            assert_eq!(revert_message(error), expected.to_string());
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
        let public = public.encode().to_vec();
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
            let scope = ExecutionScope::new();

            let mut factory = TributeFactoryContract::new(storage.clone());
            let missing = factory
                .offer_tribute(&scope, &NoParentBodies, offer(&root, &signature))
                .unwrap_err();
            assert!(revert_message(missing).contains("zkProof is required"));

            let mut wrong_root = offer(&root, &signature);
            wrong_root.zk_proof = dummy_full_proof([0x24; 32]);
            let mut factory = TributeFactoryContract::new(storage.clone());
            let mismatch = factory
                .offer_tribute(&scope, &NoParentBodies, wrong_root)
                .unwrap_err();
            assert!(revert_message(mismatch).contains("merkle_root"));
        });
    }

    #[test]
    fn unregistered_operators_cannot_offer() {
        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let scope = ExecutionScope::new();

            // An unregistered caller must fail before any enclave work.
            let mut factory = TributeFactoryContract::new(storage.clone());
            let err = factory
                .offer_tribute_with_processor(&scope, &NoParentBodies, offer(&[], &[]), |_| {
                    panic!("unregistered caller reached the enclave")
                })
                .unwrap_err();
            assert!(revert_message(err).contains("not a registered L2 operator"));
        });
    }

    /// `worldwideDay` and `tributeCurrency` are cleartext ABI arguments precisely
    /// so a bad one costs no enclave round trip. These fixtures configure no
    /// enclave client at all, so reaching the sidecar would surface as
    /// `tee_sidecar_unavailable` - the assertions below are what prove the host
    /// rejected first.
    #[test]
    fn host_rejects_an_invalid_calendar_day_before_the_enclave() {
        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let scope = ExecutionScope::new();
            let mut bad_day = signed_gate_offer(storage.clone());
            bad_day.worldwide_day = 20250230u32.into(); // February 30th

            let mut factory = TributeFactoryContract::new(storage);
            let err = factory
                .offer_tribute(&scope, &NoParentBodies, bad_day)
                .unwrap_err();
            let message = revert_message(err);
            assert!(
                message.contains("not a valid YYYYMMDD calendar date"),
                "unexpected revert: {message}"
            );
        });
    }

    /// The day check runs before the currency check, so this case needs the day to
    /// be OFFERING first - which these fixtures cannot arrange. Assert the ordering
    /// instead: an unregistered currency paired with a non-OFFERING day still
    /// reports the day, proving the currency lookup is not reached and therefore
    /// that neither reaches the enclave.
    #[test]
    fn host_rejects_a_non_offering_day_before_pricing() {
        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let scope = ExecutionScope::new();
            let mut unpriced = signed_gate_offer(storage.clone());
            unpriced.tribute_currency = 999; // never registered

            let mut factory = TributeFactoryContract::new(storage);
            let err = factory
                .offer_tribute(&scope, &NoParentBodies, unpriced)
                .unwrap_err();
            let message = revert_message(err);
            assert!(
                message.contains("is not in OFFERING status"),
                "unexpected revert: {message}"
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

    let mut metadosis = FreshDevnetGenesisBuilder::new();
    for (index, worldwide_day) in target_days.iter().copied().enumerate() {
        let offset = u64::try_from(index).unwrap() * 10;
        metadosis = metadosis.seed_active_worldwide_day(GenesisWorldwideDay {
            worldwide_day,
            status: WwdStatus::Offering,
            day_type: WwdDayType::Green,
            forming_start: offset + 1,
            forming_end: offset + 2,
            lookback_end: offset + 3,
            offering_end: offset + 4,
            scheduled_process_time: offset + 5,
            metadosis_limit_amount: U256::from(100),
            previous_vwap: U256::from(90),
            current_vwap: U256::from(100),
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

    let mut tribute = TributeContract::new(storage);
    for worldwide_day in target_days {
        tribute.unseal_day(*worldwide_day).unwrap();
    }
}

#[test]
#[ignore = "requires the pinned Barretenberg CRS"]
fn real_zk_offer_records_rewards_once_and_rolls_back_failures() {
    use commonware_codec::Encode;
    use commonware_cryptography::bls12381::primitives::{
        ops::{self, sign_message},
        variant::MinSig,
    };
    use outbe_l2registry::L2RegistryContract;
    use outbe_tee_enclave::process::{process_tribute_offer_batch, TributeOfferKeyMaterial};
    use outbe_zk_canonical::full_proof::{alloy::PublicInputs, decode_public_inputs};
    use x25519_dalek::{PublicKey, StaticSecret};

    const PROOF: &[u8] = include_bytes!(
        "../../../../testing/protocol-benchmarks/fixtures/tribute_full_proof_v1.bin"
    );
    const CALLER: Address = Address::repeat_byte(0x77);
    const REWARD_DAY: u32 = 20_260_803;
    const PRIVATE_KEY: [u8; 32] = [0x33; 32];
    outbe_zk_backend::barretenberg::init_crs().unwrap();
    let public: PublicInputs = decode_public_inputs(PROOF).unwrap().try_into().unwrap();
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
        public.merkle_root.as_slice(),
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
        exclude_from_intex_issuance: false,
        zk_proof: Bytes::from_static(PROOF),
        l2_chain_id: 0xdead,
        circuit_version: "1.1.0".to_owned(),
        zk_merkle_root: Bytes::copy_from_slice(public.merkle_root.as_slice()),
        signature: Bytes::copy_from_slice(&signature),
    };
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(date_key_to_utc_timestamp(REWARD_DAY) + 43_200));
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_offer_world(storage.clone(), &[TARGET_WWD_A]);
        let mut registry = L2RegistryContract::new(storage.clone());
        registry
            .register_network(0xdead, CALLER, &group_key.encode())
            .unwrap();
        begin_block(storage.clone(), &scope).unwrap();
    });
    let before_offer = provider.storage.clone();
    provider.fail_after_mutation_at(usize::MAX);
    StorageHandle::enter(&mut provider, |storage| {
        let mut factory = TributeFactoryContract::new(storage.clone());
        let id = factory
            .offer_tribute_with_processor(&scope, &NoParentBodies, make_offer(), |offers| {
                Ok(process_tribute_offer_batch(&key, offers).0)
            })
            .unwrap();
        let stored = TributeContract::new(storage.clone())
            .get_tribute(&scope, &NoParentBodies, id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.owner, CALLER);
        assert_eq!(stored.worldwide_day, TARGET_WWD_A);
    });
    let mutations = provider.clear_mutation_failure();
    StorageHandle::enter(&mut provider, |storage| {
        let mut factory = TributeFactoryContract::new(storage.clone());
        let replay = factory
            .offer_tribute_with_processor(&scope, &NoParentBodies, make_offer(), |offers| {
                Ok(process_tribute_offer_batch(&key, offers).0)
            })
            .unwrap_err();
        assert!(matches!(replay, PrecompileError::Revert(_)));
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
    // reward writes have begun, inside the enclosing VM transaction checkpoint.
    provider.storage = before_offer.clone();
    provider.fail_after_mutation_at(mutations - 1);
    StorageHandle::enter(&mut provider, |storage| {
        let scope = ExecutionScope::new();
        begin_block(storage.clone(), &scope).unwrap();
        storage
            .with_checkpoint(|| {
                TributeFactoryContract::new(storage.clone()).offer_tribute_with_processor(
                    &scope,
                    &NoParentBodies,
                    make_offer(),
                    |offers| Ok(process_tribute_offer_batch(&key, offers).0),
                )
            })
            .unwrap_err();
    });
    assert_eq!(provider.clear_mutation_failure(), mutations);
    assert_eq!(
        provider.storage, before_offer,
        "failed offer leaked issued or reward state"
    );
}

#[test]
fn su_hash_can_only_be_used_once() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let expect_reused = |err: PrecompileError| {
            assert!(matches!(err, PrecompileError::Revert(ref m)
                if m.contains("SU hash already used")));
        };

        // Distinct hashes across the array all succeed.
        let (a, b) = (B256::repeat_byte(0xAA), B256::repeat_byte(0xBB));
        TributeFactoryContract::new(storage.clone())
            .mark_su_hashes_used(&[a, b])
            .expect("distinct hashes ok");

        // Reuse in a later call is rejected (persistent marker).
        expect_reused(
            TributeFactoryContract::new(storage.clone())
                .mark_su_hashes_used(&[a])
                .unwrap_err(),
        );

        // Duplicate within a single array is rejected.
        let dup = B256::repeat_byte(0xCC);
        expect_reused(
            TributeFactoryContract::new(storage.clone())
                .mark_su_hashes_used(&[dup, dup])
                .unwrap_err(),
        );
    });
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
