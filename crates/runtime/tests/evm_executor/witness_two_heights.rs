//! Two executor blocks. H2 runs on H1's merged post-state.
//!
//! Each height sends five signed user transactions through `OutbeBlockExecutor`:
//! Gratis `mineCoen`, Promis `mineCoen`, Fidelity `getFidelityIndex`,
//! NOD `quoteSettlement`, and Tribute `clean`. The tape must carry a tag from
//! every confidential domain, indexed on those user transactions.

use super::*;
use alloy_consensus::TxEip1559;
use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use alloy_evm::block::BlockExecutor;
use alloy_primitives::{Bytes, TxKind, B256, U256};
use alloy_sol_types::SolCall;
use outbe_evm::config::OutbeEvmConfig;
use outbe_fidelity::precompile::IFidelity;
use outbe_gratisfactory::precompile::IGratisFactory;
use outbe_nodfactory::precompile::INodFactory;
use outbe_primitives::reshare_artifact::decode_outbe_block_artifacts;
use outbe_promisfactory::precompile::IPromisFactory;
use outbe_system_common::addresses::{
    EMISSION_ADDRESS, FIDELITY_ADDRESS, GRATIS_ADDRESS, GRATIS_FACTORY_ADDRESS,
    NOD_FACTORY_ADDRESS, PROMIS_ADDRESS, PROMIS_FACTORY_ADDRESS, REWARDS_ADDRESS, TRIBUTE_ADDRESS,
};
use outbe_system_common::time::{EmissionDate, WorldwideDay};
use outbe_tee::witness::{decode_witness_payload_v1, EnclaveWitnessEntryV1};
use outbe_tribute::precompile::ITribute;
use reth_ethereum::TransactionSigned;
use reth_evm::ConfigureEvm;
use reth_primitives_traits::Recovered;
use revm::database::states::bundle_state::BundleRetention;
use revm::Database;

const USER_TXS: usize = 5;
const MINT_MINOR: u64 = 1_000_000;
const BURN_MINOR: u64 = 1;
const DOMAIN_TAGS: [u8; 6] = [1, 2, 5, 10, 21, 13];

#[test]
fn two_blocks_record_five_domains_across_three_witness_modes() {
    use super::fixtures::{
        begin_system_txs_for_test, dummy_pubkey, post_state_root,
        state_with_active_validators_seeded, test_chain_spec, test_evm_env, test_evm_signer,
        BeginBlockFixture, BootstrapFixture, CHAIN_ID, TEST_BLOCK_TIMESTAMP_BASE,
    };

    uninstall_domain_standins();
    outbe_chain_constants::initialize(None).unwrap();
    let _ = outbe_consensus::proof::init_consensus_chain_id(CHAIN_ID);

    let leader = resident_context(1);
    let verifier = resident_context(2);
    let leader_pk = leader.attestation_public();
    assert_ne!(leader_pk, verifier.attestation_public());
    let nod_state_key = outbe_core_common::confidential_tee::derive_resident_state_key(
        &leader,
        &outbe_core_common::confidential_tee::NOD,
        0,
    )
    .expect("nod state key");
    let super::witness_quote_nod::QuoteNodSeed {
        nod_item,
        nod_id,
        user_signer,
        user_sender,
        settlement_asset,
    } = super::witness_quote_nod::quote_nod_seed(&nod_state_key, CHAIN_ID);

    let chain = B256::from(U256::from(CHAIN_ID));
    let burn = U256::from(BURN_MINOR);
    let gratis_keys = account_keys(
        &leader,
        outbe_core_common::confidential_tee::GRATIS.state_info(),
        user_sender,
    );
    let promis_keys = account_keys(
        &leader,
        outbe_core_common::confidential_tee::PROMIS.state_info(),
        user_sender,
    );
    let fidelity_sig = fidelity_signature(&user_signer, user_sender, chain);
    let tribute_day = WorldwideDay::new(20_241_221);
    let other_owner = Address::repeat_byte(0x44);
    let tribute_ids = tribute_ids(user_sender, other_owner, tribute_day);

    let signer = test_evm_signer();
    let proposer = signer.address();
    let user_tx = |nonce: u64, to: Address, input: Bytes| -> Recovered<TransactionSigned> {
        user_signer
            .sign_eip1559(TxEip1559 {
                chain_id: CHAIN_ID,
                nonce,
                gas_limit: 800_000,
                max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
                max_priority_fee_per_gas: 0,
                to: TxKind::Call(to),
                value: U256::ZERO,
                input,
                access_list: Default::default(),
            })
            .unwrap()
            .try_into_recovered()
            .unwrap()
    };
    let calls_at = |nonce_base: u64,
                    op_nonce: u64,
                    tribute: U256|
     -> Vec<Recovered<TransactionSigned>> {
        vec![
            user_tx(
                nonce_base,
                GRATIS_FACTORY_ADDRESS,
                IGratisFactory::mineCoenCall {
                    gratisMinor: burn,
                    mac: B256::from(gratis_mac(&gratis_keys, user_sender, chain, burn, op_nonce)),
                    opNonce: op_nonce,
                }
                .abi_encode()
                .into(),
            ),
            user_tx(
                nonce_base + 1,
                PROMIS_FACTORY_ADDRESS,
                IPromisFactory::mineCoenCall {
                    promisMinor: burn,
                    mac: B256::from(promis_mac(&promis_keys, user_sender, chain, burn, op_nonce)),
                    opNonce: op_nonce,
                }
                .abi_encode()
                .into(),
            ),
            user_tx(
                nonce_base + 2,
                FIDELITY_ADDRESS,
                IFidelity::getFidelityIndexCall {
                    account: user_sender,
                    expiry: TEST_BLOCK_TIMESTAMP_BASE + 100_000,
                    signature: Bytes::copy_from_slice(&fidelity_sig),
                }
                .abi_encode()
                .into(),
            ),
            user_tx(
                nonce_base + 3,
                NOD_FACTORY_ADDRESS,
                INodFactory::quoteSettlementCall {
                    nodId: nod_id,
                    asset: settlement_asset,
                }
                .abi_encode()
                .into(),
            ),
            user_tx(
                nonce_base + 4,
                TRIBUTE_ADDRESS,
                ITribute::cleanCall { tributeId: tribute }
                    .abi_encode()
                    .into(),
            ),
        ]
    };
    let user_h1 = calls_at(0, 1, tribute_ids[0]);
    let user_h2 = calls_at(USER_TXS as u64, 2, tribute_ids[1]);

    let asset_bytecode = revm::state::Bytecode::new_raw(Bytes::from_static(&[
        0x5f, 0x35, 0x60, 0xe0, 0x1c, 0x63, 0x31, 0x3c, 0xe5, 0x67, 0x14, 0x60, 0x17, 0x57, 0x61,
        0x03, 0x48, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3, 0x5b, 0x60, 0x06, 0x5f, 0x52, 0x60, 0x20,
        0x5f, 0xf3,
    ]));
    let build_seeded_state = || {
        let mut state =
            state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
                let policy = outbe_tee::witness::witness_policy();
                outbe_tee::witness::set_witness_policy_for_test(None);
                uninstall_domain_standins();
                let _seed_runtime =
                    outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(
                        std::sync::Arc::clone(&leader),
                    ));
                outbe_nod::test_support::add(&storage, &nod_item).unwrap();
                seed_minted_balances(&storage, user_sender, chain, &gratis_keys, &promis_keys);
                seed_forfeited_tributes(&storage, &leader, user_sender, other_owner, tribute_day);
                drop(_seed_runtime);
                outbe_tee::witness::set_witness_policy_for_test(policy);
                outbe_vaultrouter::schema::VaultRouterContract::new(storage.clone())
                    .asset_vault_set(settlement_asset)
                    .insert(Address::repeat_byte(0x99))
                    .unwrap();
                super::seed_valid_tee_nodes(storage.clone(), &[proposer]);
                let registry = outbe_teeregistry::TeeRegistry::new(storage);
                let node_hash = registry.validator_v1_node_hash.read(&proposer).unwrap();
                registry
                    .v1_node_attestation_ed25519
                    .write(&node_hash, B256::from(leader_pk))
                    .unwrap();
            });
        uninstall_domain_standins();
        // An empty account loses its storage at the end of the block. Marker
        // code keeps the seeded ciphertext and the vault registry across H1.
        let marker = revm::state::Bytecode::new_legacy(Bytes::from_static(&[0xef]));
        for address in [
            outbe_system_common::addresses::VAULT_ROUTER_ADDRESS,
            GRATIS_ADDRESS,
            PROMIS_ADDRESS,
            FIDELITY_ADDRESS,
            EMISSION_ADDRESS,
            TRIBUTE_ADDRESS,
        ] {
            state.database.insert_account_info(
                address,
                revm::state::AccountInfo {
                    code_hash: marker.hash_slow(),
                    code: Some(marker.clone()),
                    ..Default::default()
                },
            );
        }
        state.database.insert_account_info(
            settlement_asset,
            revm::state::AccountInfo {
                code_hash: asset_bytecode.hash_slow(),
                code: Some(asset_bytecode.clone()),
                ..Default::default()
            },
        );
        state.database.insert_account_info(
            user_sender,
            revm::state::AccountInfo {
                balance: U256::from(1_000_000_000_000_000u64),
                ..Default::default()
            },
        );
        state
    };

    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let parent_hash = B256::with_last_byte(0x11);
    // CPA stores the parent fingerprint under finalized_block_hash. H2 names a
    // different parent hash so its certificate is fresh, not a contradiction of H1.
    let h2_parent = B256::with_last_byte(0x12);
    let begin_for = |block_number: u64, height_parent: B256| {
        let (_, mut metadata) =
            super::block_two_execution_ctx(None, Bytes::new(), proposer, height_parent);
        metadata.finalized_block_number = block_number.saturating_sub(1);
        begin_system_txs_for_test(
            &config,
            BeginBlockFixture {
                block_number,
                parent_hash: height_parent,
                extra_data: &Bytes::new(),
                parent_consensus_metadata: Some(metadata),
                proposer,
                bootstrap: BootstrapFixture::StandardForBlock,
            },
        )
    };
    let begin_h1 = begin_for(2, parent_hash);
    let begin_h2 = begin_for(3, h2_parent);

    let run_height = |mut state: _,
                      block_number: u64,
                      begin: &Vec<Recovered<TransactionSigned>>,
                      user_txs: &[Recovered<TransactionSigned>],
                      is_verifier: bool,
                      extra_data: Bytes,
                      height_parent: B256| {
        let evm = config.evm_with_env(&mut state, test_evm_env(block_number, REWARDS_ADDRESS));
        let (mut ctx, _) = super::block_two_execution_ctx(
            Some(begin.len() + user_txs.len()),
            extra_data,
            proposer,
            height_parent,
        );
        if let Some(metadata) = ctx.parent_consensus_metadata.as_mut() {
            metadata.finalized_block_number = block_number.saturating_sub(1);
            metadata.finalized_block_hash = height_parent;
        }
        if is_verifier {
            ctx.block_hash = Some(B256::repeat_byte(0x9A));
            ctx.expected_begin_system_txs = begin.clone();
        }
        let mut executor = config.create_executor(evm, ctx);
        outbe_evm::executor::with_phase1_verify_disabled(|| {
            executor.apply_pre_execution_changes().unwrap();
        });
        for tx in begin.iter().cloned() {
            executor
                .execute_transaction(tx)
                .unwrap_or_else(|error| panic!("begin tx at block {block_number}: {error}"));
        }
        for (index, tx) in user_txs.iter().cloned().enumerate() {
            executor
                .execute_transaction(tx)
                .unwrap_or_else(|error| panic!("user tx {index} at block {block_number}: {error}"));
        }
        executor.apply_end_block_phase().unwrap();
        if !is_verifier {
            executor.prepare_final_header_artifacts(0).unwrap();
        }
        let extra = executor.final_extra_data.clone();
        let (evm, result) = executor.finish().unwrap();
        drop(evm);
        state.merge_transitions(BundleRetention::Reverts);
        let nonce = state
            .basic(user_sender)
            .expect("user account")
            .map(|info| info.nonce)
            .unwrap_or(0);
        let statuses: Vec<bool> = result
            .receipts
            .iter()
            .map(|receipt| receipt.status())
            .collect();
        for index in 0..user_txs.len() {
            assert!(
                statuses.get(begin.len() + index).copied().unwrap_or(false),
                "block {block_number} user tx {index} statuses {statuses:?} begin {}",
                begin.len()
            );
        }
        assert_five_domain_witness(&extra, begin.len(), block_number);
        let matched = (
            result.receipts,
            result.gas_used,
            extra,
            post_state_root(&state.bundle_state),
        );
        (state, matched, nonce)
    };

    let run_mode = |is_verifier: bool, h1_extra: Bytes, h2_extra: Bytes| {
        let state = build_seeded_state();
        let (state, h1, nonce_h1) = run_height(
            state,
            2,
            &begin_h1,
            &user_h1,
            is_verifier,
            h1_extra,
            parent_hash,
        );
        assert_eq!(nonce_h1, USER_TXS as u64, "H1 must consume five nonces");
        let (_state, h2, nonce_h2) = run_height(
            state,
            3,
            &begin_h2,
            &user_h2,
            is_verifier,
            h2_extra,
            h2_parent,
        );
        assert_eq!(
            nonce_h2,
            (USER_TXS * 2) as u64,
            "H2 must spend the nonces H1 left behind"
        );
        assert_ne!(h1.3, h2.3, "H2 post-state must differ from H1");
        (h1, h2)
    };

    outbe_tee::witness::set_witness_policy_for_test(Some(
        outbe_tee::witness::WitnessPolicy::EnclaveBacked,
    ));
    let _leader_runtime = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(
        Some(std::sync::Arc::clone(&leader)),
    );
    let (proposer_h1, proposer_h2) = run_mode(false, Bytes::new(), Bytes::new());

    drop(_leader_runtime);
    let _verifier_runtime =
        outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(verifier));
    let (enclave_h1, enclave_h2) = run_mode(true, proposer_h1.2.clone(), proposer_h2.2.clone());
    assert_eq!(proposer_h1, enclave_h1);
    assert_eq!(proposer_h2, enclave_h2);

    outbe_tee::witness::set_witness_policy_for_test(Some(
        outbe_tee::witness::WitnessPolicy::ReplayOnly,
    ));
    drop(_verifier_runtime);
    let _absent = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(None);
    let (replay_h1, replay_h2) = run_mode(true, proposer_h1.2.clone(), proposer_h2.2.clone());
    assert_eq!(proposer_h1, replay_h1);
    assert_eq!(proposer_h2, replay_h2);
    outbe_tee::witness::set_witness_policy_for_test(None);
}

fn assert_five_domain_witness(extra: &Bytes, begin_len: usize, block_number: u64) {
    let artifacts = decode_outbe_block_artifacts(extra)
        .unwrap_or_else(|error| panic!("block {block_number} artifacts: {error}"));
    let witness = artifacts
        .enclave_witness
        .unwrap_or_else(|| panic!("block {block_number} missing enclave witness"));
    let tape = decode_witness_payload_v1(&witness)
        .unwrap_or_else(|error| panic!("block {block_number} witness: {error}"));
    assert!(
        tape.prefix.is_empty(),
        "block {block_number} prefix {:?}",
        tape.prefix
    );
    let user: Vec<_> = tape
        .txs
        .iter()
        .filter(|tx| tx.tx_index >= begin_len as u32)
        .collect();
    assert_eq!(user.len(), USER_TXS, "block {block_number} user witnesses");
    let mut tags = Vec::new();
    for (offset, tx) in user.iter().enumerate() {
        assert_eq!(
            tx.tx_index,
            (begin_len + offset) as u32,
            "block {block_number}"
        );
        assert!(!tx.entries.is_empty(), "block {block_number} tx {offset}");
        for entry in &tx.entries {
            match entry {
                EnclaveWitnessEntryV1::Payload { payload, .. } => {
                    tags.push(*payload.first().expect("payload tag"));
                }
                other => panic!("block {block_number} recorded {other:?}"),
            }
        }
    }
    for tag in DOMAIN_TAGS {
        assert!(
            tags.contains(&tag),
            "block {block_number} tags {tags:?} missing domain tag {tag}"
        );
    }
}

fn uninstall_domain_standins() {
    outbe_gratis::enclave_client::test_enclave::uninstall();
    outbe_fidelity::enclave_client::test_enclave::uninstall();
    outbe_nod::test_support::uninstall();
    outbe_tribute::enclave_client::test_enclave::uninstall();
}

fn account_keys(
    leader: &outbe_tee::in_process::InProcessTeeContext,
    label: &[u8],
    account: Address,
) -> outbe_tee::resident::AccountKeysHandle {
    let state_key = leader.state_key_handle(label, 0).expect("state key");
    if label == outbe_core_common::confidential_tee::GRATIS.state_info() {
        outbe_gratis::derive_account_keys(&state_key, account).expect("gratis keys")
    } else {
        outbe_promis::derive_account_keys(&state_key, account).expect("promis keys")
    }
}

macro_rules! domain_mac {
    ($domain:ident, $op:expr, $keys:expr, $account:expr, $chain:expr, $amount:expr, $nonce:expr) => {
        $domain::modify_mac(
            $keys,
            &$domain::ModifyOperation {
                account: $account,
                op: $op,
                amount: $amount,
                op_nonce: $nonce,
                chain_id: $chain,
            },
        )
    };
}

fn gratis_mac(
    keys: &outbe_tee::resident::AccountKeysHandle,
    account: Address,
    chain_id: B256,
    amount: U256,
    op_nonce: u64,
) -> [u8; 32] {
    domain_mac!(
        outbe_gratis,
        outbe_gratis::GratisOp::Burn,
        keys,
        account,
        chain_id,
        amount,
        op_nonce
    )
}

fn promis_mac(
    keys: &outbe_tee::resident::AccountKeysHandle,
    account: Address,
    chain_id: B256,
    amount: U256,
    op_nonce: u64,
) -> [u8; 32] {
    domain_mac!(
        outbe_promis,
        outbe_promis::PromisOp::Burn,
        keys,
        account,
        chain_id,
        amount,
        op_nonce
    )
}

fn mint_mac(
    keys: &outbe_tee::resident::AccountKeysHandle,
    account: Address,
    chain_id: B256,
    amount: U256,
    gratis: bool,
) -> [u8; 32] {
    if gratis {
        domain_mac!(
            outbe_gratis,
            outbe_gratis::GratisOp::Mint,
            keys,
            account,
            chain_id,
            amount,
            0
        )
    } else {
        domain_mac!(
            outbe_promis,
            outbe_promis::PromisOp::Mint,
            keys,
            account,
            chain_id,
            amount,
            0
        )
    }
}

fn seed_minted_balances(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    account: Address,
    chain_id: B256,
    gratis_keys: &outbe_tee::resident::AccountKeysHandle,
    promis_keys: &outbe_tee::resident::AccountKeysHandle,
) {
    let amount = U256::from(MINT_MINOR);
    outbe_gratis::api::mint(
        storage.clone(),
        account,
        amount,
        outbe_gratis::ModifyAuth {
            mac: mint_mac(gratis_keys, account, chain_id, amount, true),
            op_nonce: 0,
        },
    )
    .expect("seed gratis");
    outbe_promis::api::mint_from_gem(
        storage.clone(),
        U256::ONE,
        account,
        amount,
        outbe_promis::ModifyAuth {
            mac: mint_mac(promis_keys, account, chain_id, amount, false),
            op_nonce: 0,
        },
    )
    .expect("seed promis");
}

fn fidelity_signature(
    signer: &outbe_primitives::signer::OutbeEvmSigner,
    account: Address,
    chain_id: B256,
) -> [u8; 65] {
    let message = outbe_fidelity::fidelity_query_auth_message(
        chain_id,
        account,
        super::fixtures::TEST_BLOCK_TIMESTAMP_BASE + 100_000,
    );
    let hash = outbe_core_common::confidential_tee::eip191_hash(&message);
    signer.sign_hash(&hash).expect("fidelity query signature")
}

fn tribute_ids(first_owner: Address, second_owner: Address, day: WorldwideDay) -> [U256; 2] {
    [first_owner, second_owner].map(|owner| {
        outbe_core_common::wwd_entity_id::derive_poseidon_entity_id(owner, day).to_u256()
    })
}

fn seed_forfeited_tributes(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    leader: &outbe_tee::in_process::InProcessTeeContext,
    first_owner: Address,
    second_owner: Address,
    day: WorldwideDay,
) {
    let key = leader
        .state_key_handle(outbe_core_common::confidential_tee::TRIBUTE.state_info(), 0)
        .expect("tribute state key");
    for owner in [first_owner, second_owner] {
        let tribute_id = outbe_core_common::wwd_entity_id::derive_poseidon_entity_id(owner, day);
        let body = outbe_tribute::confidential::encrypt_tribute(
            &key,
            outbe_core_common::tribute_encryption::TributeContextV2 {
                chain_id: storage.chain_id().expect("chain"),
                tribute_id,
                owner,
                worldwide_day: day,
                issuance_currency: 840,
                reference_currency: 978,
                tribute_price_minor: U256::ONE,
                offer_input_hash: B256::repeat_byte(0x5A),
            },
            &outbe_core_common::tribute_encryption::TributeAmountsV2 {
                issuance_amount_minor: U256::from(1_000u64),
                nominal_amount_minor: U256::from(100u64),
            },
        )
        .expect("encrypt tribute");
        outbe_tribute::state_api::issue_encrypted_with_league(storage, &body, Some(1))
            .expect("issue tribute");
    }
    let emission_day = EmissionDate::new(day.value());
    outbe_tribute::TributeContract::new(storage.clone())
        .day_emission_day
        .write(&day, emission_day)
        .expect("link emission day");
    outbe_emission::api::record_failed_day_allocation(
        storage.clone(),
        day.value(),
        U256::from(MINT_MINOR),
    )
    .expect("cancel emission day");
}

fn resident_context(seed: u8) -> std::sync::Arc<outbe_tee::in_process::InProcessTeeContext> {
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    use outbe_tee::resident::ResidentOfferMaterial;
    let binding = NetworkBindingV1 {
        chain_id: U256::from(super::fixtures::CHAIN_ID).to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x22),
        attestation_mode: AttestationMode::NoAttestationDev,
    };
    let context = std::sync::Arc::new(outbe_tee::test_utils::resident_context(seed, binding));
    context
        .activate_for_test(
            ResidentOfferMaterial::for_test(
                b"executor-nod-quote-group-signature-v1!".to_vec(),
                B256::from(binding.chain_id),
                0,
                0,
            )
            .expect("offer material"),
        )
        .expect("activate offer");
    context
}
