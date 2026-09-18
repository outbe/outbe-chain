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
/// The L2 and the tribute circuit version every fixture here offers under —
/// the pair that selects a verification key.
const L2_CHAIN_ID: u64 = 0xdead;
const CIRCUIT_VERSION: &str = "1.0.0";

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
    use outbe_l2_zk_canonical::claims::tribute::PUBLIC_INPUT_COUNT;
    use outbe_l2_zk_canonical::{combined_len, Claim};
    use outbe_l2registry::L2RegistryContract;
    use outbe_primitives::error::PrecompileError;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;

    use super::NoParentBodies;
    use super::{CIRCUIT_VERSION, L2_CHAIN_ID};
    use crate::runtime::OfferTributeInput;
    use crate::schema::TributeFactoryContract;

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
            circuit_version: CIRCUIT_VERSION.to_owned(),
            zk_merkle_root: Bytes::copy_from_slice(zk_merkle_root),
            signature: Bytes::copy_from_slice(signature),
        }
    }

    fn dummy_proof(root: [u8; 32]) -> Bytes {
        let vk = outbe_l2registry::api::vk_for(
            outbe_primitives::chain::DEVNET_CHAIN_ID,
            L2_CHAIN_ID,
            Claim::Tribute,
            CIRCUIT_VERSION,
        )
        .expect("the test L2 registers the fixture circuit version");
        let combined = combined_len(vk, PUBLIC_INPUT_COUNT).unwrap();
        let mut proof = Vec::with_capacity(combined);
        proof.extend_from_slice(&4u32.to_be_bytes());
        proof.extend_from_slice(&[0x01; 32]);
        proof.extend_from_slice(&[0x02; 32]);
        proof.extend_from_slice(&[0x03; 32]);
        proof.extend_from_slice(&root);
        proof.resize(combined, 0);
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
        input.zk_proof = dummy_proof(root);
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
            // Neither an empty selector nor an L1 circuit version selects a
            // key: "1.2.0" is the paynote circuit's version, registered in the
            // L1 registry and never for an L2 claim. Both must be named
            // explicitly and both must miss.
            for version in ["", "1.2.0"] {
                let mut wrong_version = offer(&root, &good_sig);
                wrong_version.zk_proof = dummy_proof(root);
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
            // The submitter need not be the registered network operator.
            valid_gate.caller = Address::repeat_byte(0x88);
            valid_gate.zk_proof = dummy_proof(root);
            let mut factory = TributeFactoryContract::new(storage.clone());
            let err = factory
                .offer_tribute(&scope, &NoParentBodies, valid_gate)
                .unwrap_err();
            assert!(revert_message(err).contains("is not in OFFERING status"));

            // A caller's registration on another chain cannot authorize an
            // offer for a chain that is no longer registered.
            registry.remove_network(caller(), L2_CHAIN_ID).unwrap();
            registry.register_network(4242, caller(), &public).unwrap();
            let mut wrong_chain = offer(&root, &good_sig);
            wrong_chain.zk_proof = dummy_proof(root);
            let error = factory
                .offer_tribute(&scope, &NoParentBodies, wrong_chain)
                .unwrap_err();
            let expected = outbe_l2registry::errors::L2RegistryError::NetworkNotRegistered {
                chain_id: L2_CHAIN_ID,
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
            wrong_root.zk_proof = dummy_proof([0x24; 32]);
            let mut factory = TributeFactoryContract::new(storage.clone());
            let mismatch = factory
                .offer_tribute(&scope, &NoParentBodies, wrong_root)
                .unwrap_err();
            assert!(revert_message(mismatch).contains("merkle_root"));
        });
    }

    #[test]
    fn unregistered_chains_cannot_offer() {
        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let scope = ExecutionScope::new();

            // An unregistered chain must fail before any enclave work.
            let mut factory = TributeFactoryContract::new(storage.clone());
            let err = factory
                .offer_tribute_with_processor(&scope, &NoParentBodies, offer(&[], &[]), |_| {
                    panic!("unregistered chain reached the enclave")
                })
                .unwrap_err();
            let expected = outbe_l2registry::errors::L2RegistryError::NetworkNotRegistered {
                chain_id: L2_CHAIN_ID,
            };
            assert_eq!(revert_message(err), expected.to_string());
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
    use outbe_l2_zk_canonical::claims::tribute::{alloy::PublicInputs, decode_public_inputs};
    use outbe_l2registry::L2RegistryContract;
    use outbe_tee_enclave::process::{process_tribute_offer_batch, TributeOfferKeyMaterial};
    use x25519_dalek::{PublicKey, StaticSecret};

    const PROOF: &[u8] =
        include_bytes!("../../../../testing/protocol-benchmarks/fixtures/tribute_offer_proof.bin");
    const CALLER: Address = Address::repeat_byte(0x77);
    const REWARD_DAY: u32 = 20_260_803;
    const PRIVATE_KEY: [u8; 32] = [0x33; 32];
    outbe_zk_backend::barretenberg::init_crs().unwrap();
    let vk = outbe_l2registry::api::vk_for(
        outbe_primitives::chain::DEVNET_CHAIN_ID,
        0xdead,
        outbe_l2_zk_canonical::Claim::Tribute,
        CIRCUIT_VERSION,
    )
    .expect("the fixture's circuit version is registered");
    let public: PublicInputs = decode_public_inputs(PROOF, vk).unwrap().try_into().unwrap();
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
        circuit_version: CIRCUIT_VERSION.to_owned(),
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
            .register_network(0xdead, Address::repeat_byte(0x88), &group_key.encode())
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

/// The `chainId` argument of `offerTribute` must reach the enclave and land in
/// `binding_hash`. Nothing else in the pipeline notices if it is dropped or
/// replaced by the host chain id: the enclave folds whatever the context says,
/// and the host then compares that fold against the enclave's own output.
///
/// The expected values below are frozen, not recomputed through `binding()`:
///
/// ```text
/// binding_hash = poseidon2([1, 0x7777…77, draft_lo128, draft_hi128,
///                           424_242 (devnet host), <chainId>])
/// ```
///
/// with `draft = 0x1111…11`, so `draft_hi128 = draft_lo128 = 0x1111…11` (16
/// bytes). Two chain ids, two frozen digests: passing the host chain id, a
/// constant, or the other L2's id all miss both.
#[test]
fn offer_tribute_folds_the_calldata_l2_chain_id_into_binding_hash() {
    use commonware_codec::Encode;
    use commonware_cryptography::bls12381::primitives::{
        ops::{self, sign_message},
        variant::MinSig,
    };
    use outbe_l2_zk_canonical::{claims::tribute::PUBLIC_INPUT_COUNT, combined_len, Claim};
    use outbe_l2registry::L2RegistryContract;
    use outbe_tee::protocol::{TributeZkContext, TributeZkExpectedHashes};
    use outbe_tee_enclave::process::{process_tribute_offer_batch, TributeOfferKeyMaterial};
    use x25519_dalek::{PublicKey, StaticSecret};

    const CALLER: Address = Address::repeat_byte(0x77);
    const PRIVATE_KEY: [u8; 32] = [0x33; 32];
    const DRAFT_ID: B256 = B256::repeat_byte(0x11);
    const SU_HASH: B256 = B256::repeat_byte(0x22);
    const ROOT: [u8; 32] = [0x04; 32];

    let payload = serde_json::to_vec(&serde_json::json!({
        "creator": format!("{CALLER:#x}"),
        "tribute_draft_id": format!("{DRAFT_ID:#x}"),
        "amount_base": "100",
        "amount_micro": "0",
        "su_hashes": [format!("{SU_HASH:#x}")],
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
        &ROOT,
    )
    .encode();

    // A well-framed envelope whose four public words are placeholders: it gets
    // past decoding and the merkle-root check, so the enclave runs, and then
    // fails the host's `nft_hash` comparison - which is after the fold and
    // before the verifier, so no CRS is needed.
    let vk = outbe_l2registry::api::vk_for(
        outbe_primitives::chain::DEVNET_CHAIN_ID,
        L2_CHAIN_ID,
        Claim::Tribute,
        CIRCUIT_VERSION,
    )
    .expect("the test L2 registers the fixture circuit version");
    let mut proof = Vec::with_capacity(combined_len(vk, PUBLIC_INPUT_COUNT).unwrap());
    proof.extend_from_slice(&4u32.to_be_bytes());
    proof.extend_from_slice(&[0x01; 32]); // owner -> context.owner
    proof.extend_from_slice(&[0x02; 32]); // nft_hash (placeholder)
    proof.extend_from_slice(&[0x03; 32]); // binding_hash (placeholder)
    proof.extend_from_slice(&ROOT);
    proof.resize(combined_len(vk, PUBLIC_INPUT_COUNT).unwrap(), 0);
    let proof = Bytes::from(proof);

    let offer_for = |l2_chain_id: u32| OfferTributeInput {
        caller: CALLER,
        cipher_text: Bytes::copy_from_slice(&cipher),
        nonce: Bytes::copy_from_slice(&nonce),
        ephemeral_pubkey: U256::from_be_bytes(ephemeral),
        worldwide_day: TARGET_WWD_A,
        tribute_currency: 840,
        reference_currency: 840,
        exclude_from_intex_issuance: false,
        zk_proof: proof.clone(),
        l2_chain_id,
        circuit_version: CIRCUIT_VERSION.to_owned(),
        zk_merkle_root: Bytes::copy_from_slice(&ROOT),
        signature: Bytes::copy_from_slice(&signature),
    };

    // One offer, observed at the enclave boundary: what the host handed over,
    // and what the enclave derived from it.
    let run = |l2_chain_id: u32| -> (TributeZkContext, TributeZkExpectedHashes) {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let scope = ExecutionScope::new();
        let observed = std::cell::RefCell::new(None);
        StorageHandle::enter(&mut provider, |storage| {
            seed_offer_world(storage.clone(), &[TARGET_WWD_A]);
            L2RegistryContract::new(storage.clone())
                .register_network(
                    u64::from(l2_chain_id),
                    Address::repeat_byte(0x88),
                    &group_key.encode(),
                )
                .unwrap();
            begin_block(storage.clone(), &scope).unwrap();

            let error = TributeFactoryContract::new(storage.clone())
                .offer_tribute_with_processor(
                    &scope,
                    &NoParentBodies,
                    offer_for(l2_chain_id),
                    |offers| {
                        let results = process_tribute_offer_batch(&key, offers).0;
                        *observed.borrow_mut() = Some((
                            offers[0].zk_context.clone().expect("zk context"),
                            results[0]
                                .zk_expected_hashes
                                .clone()
                                .expect("expected hashes"),
                        ));
                        Ok(results)
                    },
                )
                .unwrap_err();
            assert!(
                matches!(&error, PrecompileError::Revert(message) if message.contains("nft_hash")),
                "unexpected error: {error}"
            );
        });
        observed.into_inner().expect("enclave was reached")
    };

    // The expectation, spelled out from the formula rather than taken from
    // `binding()`: same preimage, written here, folded here.
    let expected_binding = |l2_chain_id: u64| {
        use outbe_l2_zk_canonical::outbe_zk_core::codec::{field_from_be_bytes, field_to_b256};
        use outbe_l2_zk_canonical::outbe_zk_core::hash::poseidon2;
        use outbe_l2_zk_canonical::outbe_zk_core::Fr;
        field_to_b256(
            &poseidon2(&[
                Fr::from(1u64), // BINDING_DOMAIN
                field_from_be_bytes(CALLER.as_slice()),
                field_from_be_bytes(&DRAFT_ID.0[16..]),
                field_from_be_bytes(&DRAFT_ID.0[..16]),
                Fr::from(CHAIN_ID),
                Fr::from(l2_chain_id),
            ])
            .unwrap(),
        )
        .unwrap()
    };

    let (context, hashes) = run(0xdead);
    assert_eq!(
        context.l2_chain_id, 0xdead,
        "calldata chainId reached the enclave"
    );
    assert_eq!(
        context.chain_id, CHAIN_ID,
        "host chain id is still its own field"
    );
    assert_eq!(hashes.binding_hash, expected_binding(0xdead));
    // Frozen too, so a change to the fold itself cannot move both sides at once.
    assert_eq!(
        hashes.binding_hash,
        "0x213ccd4535aec004fb58f0dc1be3af46de5a252a923e771cd8517b0fe53649cb"
            .parse::<B256>()
            .unwrap()
    );

    let (other_context, other) = run(0xbeef);
    assert_eq!(other_context.l2_chain_id, 0xbeef);
    assert_eq!(other.binding_hash, expected_binding(0xbeef));
    // Frozen too - the doc above promises two digests, so freeze both.
    assert_eq!(
        other.binding_hash,
        "0x005e84f521ff1759cb5022e8926f123430421e5dda8ee71c76bacc8a651a1201"
            .parse::<B256>()
            .unwrap()
    );
    assert_ne!(hashes.binding_hash, other.binding_hash);
    assert_eq!(
        hashes.nft_hash, other.nft_hash,
        "only the binding moves with the L2 id"
    );
}
