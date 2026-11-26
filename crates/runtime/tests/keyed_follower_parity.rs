//! One shared genesis and four independently imported block heights.
//! Height 1 runs OST3 bootstrap; heights 2–4 carry all five confidential domains.

#![allow(unused_imports)]

use alloy_consensus::{
    Header, Sealable as _, SignableTransaction as _, Transaction as _, TxEip1559, TxReceipt as _,
};
use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use alloy_primitives::{
    address, keccak256, logs_bloom, Address, Bytes, Signature, TxKind, B256, U256,
};
use alloy_sol_types::SolCall;
use evm2::{
    block::{BundleSource, BundleState},
    bytecode::Bytecode,
    env::BlockEnvExt,
    evm::{AccountInfo, CacheDB, Database as _, EmptyDB},
    SpecId,
};
use outbe_evm::config::{OutbeBlockExecutionCtx, OutbeEvmConfig};
use outbe_evm::executor::*;
use outbe_evm::factory::OutbeEvmTypes;
use outbe_evm::signer::{OutbeEvmSigner, SharedOutbeEvmSigner};
use outbe_evm::system_tx::SystemTx;
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::reshare_artifact::{
    decode_outbe_block_artifacts, encode_outbe_block_artifacts, ConsensusHeaderArtifact,
    ExecutionSummaryArtifact,
};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::tee_genesis_v1::NO_ATTESTATION_DEV_CHAIN_ID;
use outbe_primitives::OutbeHeader;
use outbe_system_common::addresses::{
    CYCLE_ADDRESS, NOD_ADDRESS, ORACLE_ADDRESS, REWARDS_ADDRESS, SLASH_INDICATOR_ADDRESS,
    STABLECOIN_FACTORY_ADDRESS, STABLECOIN_POLICY_REGISTRY_ADDRESS, STAKING_ADDRESS,
    UPDATE_ADDRESS, VOTE_ADDRESS,
};
use reth_ethereum::chainspec::{ChainSpec, MAINNET};
use reth_ethereum::evm::{EthBlockExecutionCtx, EthEvmEnv};
use reth_ethereum::{Receipt, TransactionSigned};
use reth_evm::execute::{BlockExecutionError, BlockExecutor as _};
use reth_evm::{BlockExecutorFactory as _, RecoveredTx as _};
use reth_primitives_traits::{Recovered, SealedHeader, SignedTransaction as _};
use std::sync::Arc;

#[allow(dead_code, unused_imports, clippy::all)]
#[path = "evm_executor/fixtures.rs"]
mod fixtures;
#[allow(dead_code, unused_imports, clippy::all)]
#[path = "evm_executor/fixtures_execution.rs"]
mod fixtures_execution;
#[allow(dead_code, unused_imports, clippy::all)]
#[path = "evm_executor/witness_quote_nod.rs"]
mod witness_quote_nod;

#[path = "tee_vm_lifecycle_supervisor/execution_committee.rs"]
mod execution_committee;
#[path = "keyed_follower_parity/finalized.rs"]
mod finalized;
#[path = "support/follower_import.rs"]
mod follower_import;

#[path = "keyed_follower_parity/bootstrap.rs"]
mod bootstrap;
#[path = "keyed_follower_parity/execution.rs"]
mod execution;
use execution::{assert_linked, committee_snapshot, produce};
use finalized::ReplayRole;

use fixtures::{
    begin_system_txs_for_test, block_one_artifacts, block_one_execution_ctx,
    block_two_execution_ctx, dummy_pubkey, seed_valid_tee_nodes,
    state_with_active_validators_seeded, test_chain_spec, test_evm_env, test_evm_signer,
    BeginBlockFixture, BootstrapFixture, CHAIN_ID, TEST_BLOCK_TIMESTAMP_BASE,
};

const HEIGHTS: u64 = 3;
const FIRST_BLOCK: u64 = 1;
const USER_TXS: usize = 4;
const MINT_MINOR: u64 = 1_000_000;
const BURN_MINOR: u64 = 1;
const DOMAIN_TAGS: [u8; 5] = [1, 2, 5, 10, 21];
const OFFER: &[u8] = b"executor-nod-quote-group-signature-v1!";

type Signed = Recovered<TransactionSigned>;
type ExecState = CacheDB<EmptyDB>;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct HeightSeal {
    hash: B256,
    state_root: B256,
    extra: Bytes,
}

struct Prepared {
    config: OutbeEvmConfig,
    spec: Arc<ChainSpec<OutbeHeader>>,
    proposer: Address,
    verifier: Address,
    keyed_address: Address,
    user_sender: Address,
    nod_item: outbe_nod::NodItemState,
    user_txs: Vec<Vec<Signed>>,
    settlement_asset: Address,
    asset_bytecode: Bytecode,
    leader: Arc<outbe_tee::in_process::InProcessTeeContext>,
    keyed: Arc<outbe_tee::in_process::InProcessTeeContext>,
    gratis_keys: outbe_tee::resident::AccountKeysHandle,
    promis_keys: outbe_tee::resident::AccountKeysHandle,
    committee: execution_committee::Committee,
}

struct Produced {
    seals: Vec<HeightSeal>,
    blocks: Vec<outbe_primitives::OutbeBlock>,
    parents: Vec<B256>,
}

#[test]
fn keyed_full_node_matches_committee_and_keyless_follower_for_three_heights() {
    if let Some(datadir) = std::env::var_os(finalized::KEYLESS_CHILD_DIR) {
        finalized::run_keyless_child(std::path::Path::new(&datadir));
        return;
    }
    uninstall_domain_standins();
    outbe_chain_constants::initialize(None).unwrap();
    let _ = outbe_consensus::proof::init_consensus_chain_id(CHAIN_ID);
    let initial_spec = test_chain_spec();
    let provisional = prepare(
        &resident_context(1, initial_spec.genesis_hash()),
        &resident_context(3, initial_spec.genesis_hash()),
        initial_spec,
    );
    let spec = finalized::chain_spec(&seeded_state(&provisional));
    let leader = resident_context(1, spec.genesis_hash());
    let verifier_ctx = resident_context(9, spec.genesis_hash());
    let keyed = resident_context(3, spec.genesis_hash());
    assert_ne!(leader.attestation_public(), keyed.attestation_public());
    assert_ne!(
        leader.manifest().enclave_id().unwrap(),
        keyed.manifest().enclave_id().unwrap()
    );
    assert_eq!(
        leader.network_encryption_public().unwrap(),
        keyed.network_encryption_public().unwrap()
    );
    let prepared = prepare(&leader, &keyed, spec);
    assert_eq!(
        prepared.spec.genesis_header().inner.state_root,
        finalized::state_root(&seeded_state(&prepared)),
        "resident identities bound to the sealed chain leave the genesis allocation unchanged",
    );
    let nod_key = outbe_core_common::confidential_tee::derive_resident_state_key(
        &keyed,
        &outbe_core_common::confidential_tee::NOD,
        0,
    )
    .unwrap();
    assert_eq!(
        outbe_nod::nod_encryption::decrypt_nod(&nod_key, &prepared.nod_item.encrypted).unwrap(),
        U256::from(MINT_MINOR)
    );
    assert!(
        !outbe_consensus::stack::epoch::signer::radicle_signer_enabled(
            outbe_radicle::integration::RadicleVotingGate::SignerAllowed,
            false,
        )
        .unwrap()
    );

    outbe_tee::witness::set_witness_policy_for_test(Some(
        outbe_tee::witness::WitnessPolicy::EnclaveBacked,
    ));
    let _leader_guard = install(Some(Arc::clone(&leader)));
    let committee = produce(&prepared);
    assert_linked(&committee);
    for block in &committee.blocks {
        execution::committee_finalization(
            &prepared,
            block.header.inner.number,
            block.header.inner.hash_slow(),
        );
    }
    drop(_leader_guard);
    outbe_tee::witness::set_witness_policy_for_test(None);
    let dirs = tempfile::tempdir().unwrap();
    let keyless = finalized::replay_keyless_process(
        &prepared.spec,
        &committee.blocks,
        &dirs.path().join("keyless"),
        "keyed_full_node_matches_committee_and_keyless_follower_for_three_heights",
    );
    outbe_tee::witness::install_witness_policy(outbe_tee::witness::WitnessPolicy::EnclaveBacked)
        .unwrap();
    outbe_tee::install_in_process_tee_context(Arc::clone(&verifier_ctx)).unwrap();
    let committee_finalized = finalized::replay(
        &prepared.spec,
        &committee.blocks,
        &dirs.path().join("committee-verifier"),
        ReplayRole::CommitteeVerifier,
    );
    assert_eq!(committee.seals, committee_finalized);
    assert_eq!(committee_finalized, keyless);
    outbe_tee::replace_in_process_tee_context::<outbe_tee::InProcessDispatchError>(
        &verifier_ctx,
        Arc::clone(&keyed),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(
        committee_finalized,
        finalized::replay(
            &prepared.spec,
            &committee.blocks,
            &dirs.path().join("keyed"),
            ReplayRole::Keyed(&keyed),
        ),
    );
}

fn install(
    context: Option<Arc<outbe_tee::in_process::InProcessTeeContext>>,
) -> outbe_tee::in_process_dispatch::InProcessTestGuard {
    outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(context)
}

fn prepare(
    leader: &Arc<outbe_tee::in_process::InProcessTeeContext>,
    keyed: &Arc<outbe_tee::in_process::InProcessTeeContext>,
    spec: Arc<ChainSpec<OutbeHeader>>,
) -> Prepared {
    let verifier = OutbeEvmSigner::from_secret_bytes([9; 32])
        .unwrap()
        .address();
    let nod_state_key = outbe_core_common::confidential_tee::derive_resident_state_key(
        leader,
        &outbe_core_common::confidential_tee::NOD,
        0,
    )
    .unwrap();
    let witness_quote_nod::QuoteNodSeed {
        nod_item,
        nod_id,
        user_signer,
        user_sender,
        settlement_asset,
    } = witness_quote_nod::quote_nod_seed(&nod_state_key, CHAIN_ID);
    let chain = B256::from(U256::from(CHAIN_ID));
    let burn = U256::from(BURN_MINOR);
    let gratis_keys = account_keys(
        leader,
        outbe_core_common::confidential_tee::GRATIS.state_info(),
        user_sender,
    );
    let promis_keys = account_keys(
        leader,
        outbe_core_common::confidential_tee::PROMIS.state_info(),
        user_sender,
    );
    let fidelity_sig = fidelity_signature(&user_signer, user_sender, chain);
    let signer = test_evm_signer();
    let user_tx = |nonce, to, input| sign_user(&user_signer, nonce, to, input);
    let user_txs = std::iter::once(Vec::new())
        .chain((0..HEIGHTS).map(|height| {
            let base = height * USER_TXS as u64;
            let op = height + 1;
            vec![
                user_tx(
                    base,
                    gratis_factory(),
                    mine(
                        outbe_core_common::confidential_tee::Ledger::Gratis,
                        &gratis_keys,
                        (user_sender, chain, burn, op),
                    ),
                ),
                user_tx(
                    base + 1,
                    promis_factory(),
                    mine(
                        outbe_core_common::confidential_tee::Ledger::Promis,
                        &promis_keys,
                        (user_sender, chain, burn, op),
                    ),
                ),
                user_tx(
                    base + 2,
                    fidelity(),
                    fidelity_call(user_sender, &fidelity_sig),
                ),
                user_tx(base + 3, nod_factory(), nod_call(nod_id, settlement_asset)),
            ]
        }))
        .collect();
    Prepared {
        config: OutbeEvmConfig::new(
            Arc::clone(&spec),
            Arc::new(outbe_runtime::OutbeRuntime::new()),
        )
        .with_evm_signer(signer.clone()),
        spec,
        proposer: signer.address(),
        verifier,
        keyed_address: outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes([8; 32])
            .unwrap()
            .address(),
        user_sender,
        nod_item,
        user_txs,
        settlement_asset,
        asset_bytecode: asset_bytecode(),
        leader: Arc::clone(leader),
        keyed: Arc::clone(keyed),
        gratis_keys,
        promis_keys,
        committee: execution_committee::Committee::new(21),
    }
}

fn seeded_state(prepared: &Prepared) -> ExecState {
    let keyed_address = prepared.keyed_address;
    let proposer = prepared.proposer;
    let verifier = prepared.verifier;
    let validators = committee_snapshot(prepared)
        .committee
        .into_iter()
        .map(|entry| (entry.address, entry.consensus_pubkey))
        .collect::<Vec<_>>();
    let mut state = state_with_active_validators_seeded(&validators, |storage| {
        let policy = outbe_tee::witness::witness_policy();
        outbe_tee::witness::set_witness_policy_for_test(None);
        uninstall_domain_standins();
        let _seed = install(Some(Arc::clone(&prepared.leader)));
        outbe_nod::test_support::add(&storage, &prepared.nod_item).unwrap();
        seed_minted(
            &storage,
            prepared.user_sender,
            &prepared.gratis_keys,
            &prepared.promis_keys,
        );
        drop(_seed);
        outbe_tee::witness::set_witness_policy_for_test(policy);
        outbe_vaultrouter::schema::VaultRouterContract::new(storage.clone())
            .asset_vault_set(prepared.settlement_asset)
            .insert(Address::repeat_byte(0x99))
            .unwrap();
        write_keyed_lease(storage.clone(), &prepared.keyed);
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage);
        assert!(validators.is_consensus_participant(proposer).unwrap());
        assert!(validators.is_consensus_participant(verifier).unwrap());
        assert!(!validators.is_consensus_participant(keyed_address).unwrap());
        assert!(validators
            .get_reshare_target_set()
            .unwrap()
            .iter()
            .all(|row| row.validator_address != keyed_address));
    });
    uninstall_domain_standins();
    let marker = Bytecode::new_legacy(Bytes::from_static(&[0xef]));
    for address in marker_accounts() {
        state.insert_account_info(
            &address,
            AccountInfo {
                code_hash: marker.hash_slow(),
                code: Some(marker.clone()),
                ..Default::default()
            },
        );
    }
    state.insert_account_info(
        &prepared.settlement_asset,
        AccountInfo {
            code_hash: prepared.asset_bytecode.hash_slow(),
            code: Some(prepared.asset_bytecode.clone()),
            ..Default::default()
        },
    );
    state.insert_account_info(
        &prepared.user_sender,
        AccountInfo {
            balance: U256::from(1_000_000_000_000_000u64),
            ..Default::default()
        },
    );
    state
}

fn write_keyed_lease(
    storage: StorageHandle<'_>,
    keyed: &outbe_tee::in_process::InProcessTeeContext,
) {
    let manifest = keyed.manifest();
    let node_hash = manifest.node_id.node_id_hash().unwrap();
    let registry = outbe_teeregistry::TeeRegistry::new(storage);
    registry
        .v1_node_enclave_id
        .write(&node_hash, manifest.enclave_id().unwrap())
        .unwrap();
    registry
        .v1_node_binding_id
        .write(&node_hash, B256::repeat_byte(0x12))
        .unwrap();
    registry
        .v1_node_intent_hash
        .write(&node_hash, B256::repeat_byte(0x13))
        .unwrap();
    registry
        .v1_node_valid_until
        .write(&node_hash, u64::MAX)
        .unwrap();
}

fn assert_five_domain_witness(extra: &Bytes, begin_len: usize, block_number: u64) {
    let artifacts = decode_outbe_block_artifacts(extra)
        .unwrap_or_else(|error| panic!("block {block_number}: {error}"));
    let witness = artifacts
        .enclave_witness
        .unwrap_or_else(|| panic!("block {block_number} witness"));
    let tape = outbe_tee::witness::decode_witness_payload_v1(&witness).unwrap();
    assert!(tape.prefix.is_empty());
    let user: Vec<_> = tape
        .txs
        .iter()
        .filter(|tx| tx.tx_index >= begin_len as u32)
        .collect();
    assert_eq!(user.len(), USER_TXS);
    let mut tags = Vec::new();
    for (offset, tx) in user.iter().enumerate() {
        assert_eq!(tx.tx_index, (begin_len + offset) as u32);
        for entry in &tx.entries {
            match entry {
                outbe_tee::witness::EnclaveWitnessEntryV1::Payload { payload, .. } => {
                    tags.push(*payload.first().unwrap())
                }
                other => panic!("block {block_number} {other:?}"),
            }
        }
    }
    for tag in DOMAIN_TAGS {
        assert!(tags.contains(&tag), "block {block_number} {tags:?}");
    }
}

fn sign_user(
    signer: &outbe_primitives::signer::OutbeEvmSigner,
    nonce: u64,
    to: Address,
    input: Bytes,
) -> Signed {
    signer
        .sign_eip1559(TxEip1559 {
            chain_id: CHAIN_ID,
            nonce,
            gas_limit: 800_000,
            max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
            to: TxKind::Call(to),
            input,
            ..Default::default()
        })
        .unwrap()
        .try_into_recovered()
        .unwrap()
}

fn mine(
    ledger: outbe_core_common::confidential_tee::Ledger,
    keys: &outbe_tee::resident::AccountKeysHandle,
    query: (Address, B256, U256, u64),
) -> Bytes {
    use outbe_core_common::confidential_tee::Ledger;
    let (account, chain, amount, op) = query;
    let data = match ledger {
        Ledger::Gratis => outbe_gratisfactory::precompile::IGratisFactory::mineCoenCall {
            gratisMinor: amount,
            mac: B256::from(gratis_mac(keys, account, chain, amount, op)),
            opNonce: op,
        }
        .abi_encode(),
        Ledger::Promis => outbe_promisfactory::precompile::IPromisFactory::mineCoenCall {
            promisMinor: amount,
            mac: B256::from(promis_mac(keys, account, chain, amount, op)),
            opNonce: op,
        }
        .abi_encode(),
        _ => unreachable!("parity amount is only supplied by Gratis or Promis"),
    };
    data.into()
}

fn fidelity_call(account: Address, signature: &[u8; 65]) -> Bytes {
    outbe_fidelity::precompile::IFidelity::fidelityIndexCall {
        account,
        expiry: TEST_BLOCK_TIMESTAMP_BASE + 100_000,
        signature: Bytes::copy_from_slice(signature),
    }
    .abi_encode()
    .into()
}
fn nod_call(nod_id: U256, asset: Address) -> Bytes {
    outbe_nodfactory::precompile::INodFactory::quoteSettlementCall {
        nodId: nod_id,
        asset,
    }
    .abi_encode()
    .into()
}
fn gratis_factory() -> Address {
    outbe_system_common::addresses::GRATIS_FACTORY_ADDRESS
}
fn promis_factory() -> Address {
    outbe_system_common::addresses::PROMIS_FACTORY_ADDRESS
}
fn fidelity() -> Address {
    outbe_system_common::addresses::FIDELITY_ADDRESS
}
fn nod_factory() -> Address {
    outbe_system_common::addresses::NOD_FACTORY_ADDRESS
}
fn marker_accounts() -> [Address; 5] {
    use outbe_system_common::addresses::{
        EMISSION_ADDRESS, FIDELITY_ADDRESS, GRATIS_ADDRESS, PROMIS_ADDRESS, VAULT_ROUTER_ADDRESS,
    };
    [
        VAULT_ROUTER_ADDRESS,
        GRATIS_ADDRESS,
        PROMIS_ADDRESS,
        FIDELITY_ADDRESS,
        EMISSION_ADDRESS,
    ]
}

fn uninstall_domain_standins() {
    outbe_gratis::enclave_client::test_enclave::uninstall();
    outbe_fidelity::enclave_client::test_enclave::uninstall();
    outbe_nod::test_support::uninstall();
}

fn account_keys(
    leader: &outbe_tee::in_process::InProcessTeeContext,
    label: &[u8],
    account: Address,
) -> outbe_tee::resident::AccountKeysHandle {
    let state_key = leader.state_key_handle(label, 0).unwrap();
    if label == outbe_core_common::confidential_tee::GRATIS.state_info() {
        outbe_gratis::derive_account_keys(&state_key, account).unwrap()
    } else {
        outbe_promis::derive_account_keys(&state_key, account).unwrap()
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
fn seed_minted(
    storage: &StorageHandle<'_>,
    account: Address,
    gratis_keys: &outbe_tee::resident::AccountKeysHandle,
    promis_keys: &outbe_tee::resident::AccountKeysHandle,
) {
    let amount = U256::from(MINT_MINOR);
    let chain_id = B256::from(U256::from(CHAIN_ID));
    outbe_gratis::api::mint(
        storage.clone(),
        account,
        amount,
        outbe_gratis::ModifyAuth {
            mac: domain_mac!(
                outbe_gratis,
                outbe_gratis::GratisOp::Mint,
                gratis_keys,
                account,
                chain_id,
                amount,
                0
            ),
            op_nonce: 0,
        },
    )
    .unwrap();
    outbe_promis::api::mint_from_gem(
        storage.clone(),
        U256::ONE,
        account,
        amount,
        outbe_promis::ModifyAuth {
            mac: domain_mac!(
                outbe_promis,
                outbe_promis::PromisOp::Mint,
                promis_keys,
                account,
                chain_id,
                amount,
                0
            ),
            op_nonce: 0,
        },
    )
    .unwrap();
}
fn fidelity_signature(
    signer: &outbe_primitives::signer::OutbeEvmSigner,
    account: Address,
    chain_id: B256,
) -> [u8; 65] {
    let message = outbe_fidelity::fidelity_query_auth_message(
        chain_id,
        account,
        TEST_BLOCK_TIMESTAMP_BASE + 100_000,
    );
    signer
        .sign_hash(&outbe_core_common::confidential_tee::eip191_hash(&message))
        .unwrap()
}
fn resident_context(
    seed: u8,
    genesis_hash: B256,
) -> Arc<outbe_tee::in_process::InProcessTeeContext> {
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    use outbe_tee::resident::ResidentOfferMaterial;
    let binding = NetworkBindingV1 {
        chain_id: U256::from(CHAIN_ID).to_be_bytes(),
        genesis_hash,
        attestation_mode: AttestationMode::NoAttestationDev,
    };
    let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding));
    context
        .activate_for_test(
            ResidentOfferMaterial::for_test(OFFER.to_vec(), B256::from(binding.chain_id), 0, 0)
                .unwrap(),
        )
        .unwrap();
    context
}
fn asset_bytecode() -> Bytecode {
    Bytecode::new_legacy(Bytes::from_static(&[
        0x5f, 0x35, 0x60, 0xe0, 0x1c, 0x63, 0x31, 0x3c, 0xe5, 0x67, 0x14, 0x60, 0x17, 0x57, 0x61,
        0x03, 0x48, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3, 0x5b, 0x60, 0x06, 0x5f, 0x52, 0x60, 0x20,
        0x5f, 0xf3,
    ]))
}
