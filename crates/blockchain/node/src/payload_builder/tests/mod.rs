mod carrier_admission;
mod size_budget;
mod stages;

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

use super::*;
use alloy_consensus::{SignableTransaction as _, Transaction as _, TxEip1559};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{address, keccak256, Bytes, Signature, TxKind, U256};
use alloy_rpc_types_engine::PayloadId;
use outbe_compressed_entities::{
    CandidateCacheLimits, CeMdbx, CompressedTreeService, EnvironmentIdentity, ExactParentIdentity,
    FinalizedMarker, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_primitives::runtime_audit_v1::{
    BODY_READ_REQUEST_DEADLINE, OTHER_PAYLOAD_EXECUTION_FAILURE,
};

use outbe_evm::{
    system_tx::{split_system_layout, system_tx_intrinsic_gas, SystemTxKind},
    OutbeEvmSigner,
};
use outbe_metadosis::test_support::ForkInstallScenario;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, REWARDS_ADDRESS},
    block::{BlockContext, BlockRuntimeContext},
    consensus::{ConsensusExecutionBridge, DkgBoundaryArtifact, ReshareResult},
    projection::ExecutionReadBudget,
    reshare_artifact::{
        encode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
    },
    storage::{hashmap::HashMapStorageProvider, MetadosisMutationPurposeTag, StorageHandle},
    tee_genesis_v1::GRAMINE_DIRECT_DEV_CHAIN_ID,
    units::SCALE_1E6_U256,
    OutbePrimitives,
};
use reth_chainspec::{ChainSpecBuilder, EthereumHardfork, ForkCondition};
use reth_evm::{execute::Executor as _, RecoveredTx};
use reth_payload_primitives::BuiltPayload as _;
use reth_primitives_traits::{SealedHeader, SignedTransaction as _};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
use reth_transaction_pool::{
    error::InvalidPoolTransactionError,
    identifier::{SenderId, TransactionId},
    noop::NoopTransactionPool,
    EthPooledTransaction, TransactionOrigin,
};

type TestPool = NoopTransactionPool<EthPooledTransaction>;
type TestProvider = MockEthProvider<OutbePrimitives, ChainSpec<OutbeHeader>>;
const TEST_CONSENSUS_PUBLIC_KEY: [u8; 48] = [0x11; 48];

#[test]
fn payload_failure_kind_is_structural_and_stable() {
    let deadline = BlockExecutionError::other(PrecompileError::BodyReadRequestDeadline);
    let other = BlockExecutionError::msg("unclassified payload failure");

    assert_eq!(
        payload_execution_failure_kind(&deadline),
        BODY_READ_REQUEST_DEADLINE
    );
    assert_eq!(
        payload_execution_failure_kind(&other),
        OTHER_PAYLOAD_EXECUTION_FAILURE
    );
}

struct TestBestTransactions {
    transactions: std::vec::IntoIter<Arc<ValidPoolTransaction<EthPooledTransaction>>>,
    rejected: Arc<AtomicUsize>,
}

impl TestBestTransactions {
    fn one(
        transaction: Arc<ValidPoolTransaction<EthPooledTransaction>>,
        rejected: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            transactions: vec![transaction].into_iter(),
            rejected,
        }
    }
}

impl Iterator for TestBestTransactions {
    type Item = Arc<ValidPoolTransaction<EthPooledTransaction>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.transactions.next()
    }
}

impl BestTransactions for TestBestTransactions {
    fn mark_invalid(&mut self, _transaction: &Self::Item, kind: InvalidPoolTransactionError) {
        assert!(
            matches!(kind, InvalidPoolTransactionError::ExceedsGasLimit(_, _)),
            "boundary transaction must only be rejected by the gas reservation: {kind}"
        );
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    fn no_updates(&mut self) {}

    fn set_skip_blobs(&mut self, _skip_blobs: bool) {}
}

fn active_set_hash(addresses: &[alloy_primitives::Address]) -> B256 {
    let mut bytes = Vec::with_capacity(8 + addresses.len() * 20);
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    keccak256(bytes)
}

fn genesis_boundary_artifacts(proposer: alloy_primitives::Address) -> (Bytes, B256) {
    let active_set = vec![proposer];
    let vrf_group_public_key_bytes = vec![0x42u8; 96];
    let snapshot = outbe_validatorset::CommitteeSnapshot {
        committee: vec![outbe_validatorset::CommitteeEntry {
            address: proposer,
            consensus_pubkey: TEST_CONSENSUS_PUBLIC_KEY,
        }],
        vrf_material_version: 0,
        vrf_group_public_key_bytes: vrf_group_public_key_bytes.clone(),
        vrf_public_polynomial_hash: B256::ZERO,
    };
    let committee_snapshot_hash = outbe_validatorset::committee_set_hash_v2(0, &snapshot);
    let artifacts = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: None,
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
            DkgBoundaryArtifact {
                epoch: 0,
                dkg_cycle: 0,
                freeze_height: 0,
                planned_activation_height: 1,
                target_set_hash: B256::ZERO,
                vrf_material_version: 0,
                vrf_group_public_key: keccak256(&vrf_group_public_key_bytes),
                vrf_group_public_key_bytes: Bytes::from(vrf_group_public_key_bytes),
                committee_set_hash: committee_snapshot_hash,
                is_validator_set_change: true,
                outcome: Bytes::new(),
                is_full_dkg: false,
                tee_recipient_pubkeys: Vec::new(),
                tee_expired_target_exclusions: Vec::new(),
                tee_expired_target_exclusions_hash: B256::ZERO,
                reshare: ReshareResult {
                    active_set_hash: active_set_hash(&active_set),
                    new_active_set: active_set,
                },
            },
        )),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        compressed_entities_root: None,
    })
    .expect("genesis boundary artifacts encode");
    (artifacts, committee_snapshot_hash)
}

fn genesis_dev_tee_bootstrap(
    chain_id: u64,
    genesis_hash: B256,
    committee_snapshot_hash: B256,
) -> outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2 {
    use outbe_primitives::tee_test_utils::{
        gramine_direct_bootstrap_v2, gramine_direct_policy_v1, DevValidatorV1,
    };

    let policy = gramine_direct_policy_v1(chain_id, genesis_hash)
        .expect("test GramineDirectDev policy is canonical");
    gramine_direct_bootstrap_v2(
        policy,
        committee_snapshot_hash,
        1,
        ACTIVE_PAYLOAD_BLOCK_TIMESTAMP + 7_200,
        &[DevValidatorV1 {
            evm_secret: [1; 32],
            bls_minpk_public: TEST_CONSENSUS_PUBLIC_KEY,
        }],
    )
    .expect("test GramineDirectDev OST3 payload is canonical")
}

fn active_ocomp_provider(
    chain_spec: &Arc<ChainSpec<OutbeHeader>>,
    proposer: alloy_primitives::Address,
    funded_user: alloy_primitives::Address,
    genesis_hash: B256,
) -> TestProvider {
    const OWNER: alloy_primitives::Address = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

    let mut seed =
        HashMapStorageProvider::new_with_chain_identity(chain_spec.chain().id(), genesis_hash);
    seed.set_block_number(1);
    seed.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::ForkProfile);
    StorageHandle::enter(&mut seed, |storage| {
        let root = outbe_compressed_entities::sealed_root(B256::ZERO)
            .expect("CE genesis root is deterministic");
        storage
            .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
            .expect("CE schema version seed succeeds");
        storage
            .sstore(
                COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(root.as_slice()),
            )
            .expect("CE genesis root seed succeeds");

        let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        validators
            .config_owner
            .write(OWNER)
            .expect("validator owner seed succeeds");
        validators
            .config_max_validators
            .write(128)
            .expect("validator capacity seed succeeds");
        validators
            .config_epoch_length_blocks
            .write(60)
            .expect("validator epoch seed succeeds");
        validators
            .config_is_initialized
            .write(true)
            .expect("validator initialization seed succeeds");
        validators
            .register_validator(OWNER, proposer, &TEST_CONSENSUS_PUBLIC_KEY)
            .expect("proposer registration seed succeeds");
        validators
            .activate_validator_via_boundary_for_test(proposer)
            .expect("active proposer reaches production boundary activation");
        let founder_registration = validators
            .ocomp_registration(proposer)
            .expect("active proposer OCOMP registration is readable")
            .expect("active proposer has OCOMP registration");

        let mut install =
            ForkInstallScenario::measurement_at(1, chain_spec.chain().id(), genesis_hash)
                .expect("fresh-devnet OCOMP install fixture is canonical")
                .into_install();
        install.founder_registrations = vec![founder_registration];
        let install_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                1,
                ACTIVE_PAYLOAD_BLOCK_TIMESTAMP,
                chain_spec.chain().id(),
            ),
            storage.clone(),
        );
        // Fork installation validates the required day-type pair, so the
        // test genesis must register it before installing the profile.
        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .expect("oracle pair seed succeeds");
        outbe_metadosis::commands::install_fork_profile(&install_ctx, &install)
            .expect("fresh-devnet OCOMP profile installs through the production command");

        outbe_oracle::api::set_exchange_rate(
            storage,
            alloy_primitives::Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            SCALE_1E6_U256,
            0,
            0,
        )
        .expect("oracle rate seed succeeds");
    });

    let provider =
        MockEthProvider::<OutbePrimitives>::new().with_chain_spec(chain_spec.as_ref().clone());
    let mut accounts: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for ((account, slot), value) in seed.storage {
        accounts
            .entry(account)
            .or_default()
            .push((B256::from(slot.to_be_bytes::<32>()), value));
    }
    for (account, storage) in accounts {
        provider.add_account(
            account,
            ExtendedAccount::new(0, U256::ZERO)
                .with_bytecode(Bytes::from_static(&[0xef]))
                .extend_storage(storage),
        );
    }
    provider.add_account(
        funded_user,
        ExtendedAccount::new(0, U256::from(100_000_000_000_000_000_000u128)),
    );
    provider
}

const ACTIVE_PAYLOAD_BLOCK_GAS_LIMIT: u64 = 500_000_000;
const ACTIVE_PAYLOAD_BLOCK_TIMESTAMP: u64 = 1_700_000_000;

struct ActivePayloadCase {
    payloads: Vec<OutbeBuiltPayload>,
    evm_config: OutbeEvmConfig,
    provider: TestProvider,
    rejected: Arc<AtomicUsize>,
}

fn build_active_payload_case(
    user_gas_over_boundary: u64,
    proposal_attempts: usize,
) -> ActivePayloadCase {
    assert!(
        proposal_attempts > 0,
        "test must build at least one proposal"
    );
    use outbe_primitives::tee_test_utils::{
        gramine_direct_policy_v1, tee_attestation_v1_extra_field,
    };

    let mut chain_spec = ChainSpecBuilder::mainnet()
        .with_fork(EthereumHardfork::Paris, ForkCondition::Block(0))
        .build();
    chain_spec.chain = GRAMINE_DIRECT_DEV_CHAIN_ID.into();
    chain_spec.genesis.config.chain_id = GRAMINE_DIRECT_DEV_CHAIN_ID;
    let policy = gramine_direct_policy_v1(chain_spec.chain().id(), chain_spec.genesis_hash())
        .expect("test GramineDirectDev policy is canonical");
    chain_spec.genesis.config.extra_fields.insert(
        "teeAttestationV1".to_owned(),
        tee_attestation_v1_extra_field(&policy).expect("test TEE activation manifest is canonical"),
    );
    let chain_spec: Arc<ChainSpec<OutbeHeader>> = chain_spec.map_header(OutbeHeader::new).into();
    let signer =
        Arc::new(OutbeEvmSigner::from_secret_bytes([1u8; 32]).expect("test proposer key is valid"));
    let proposer = signer.address();
    let (prefinal_extra_data, committee_snapshot_hash) = genesis_boundary_artifacts(proposer);
    let bridge = ConsensusExecutionBridge::new();
    let bootstrap = genesis_dev_tee_bootstrap(
        chain_spec.chain().id(),
        chain_spec.genesis_hash(),
        committee_snapshot_hash,
    );
    bridge.set_pending_tee_bootstrap(bootstrap.clone());
    let body_storage: StorageReaderHandle = Arc::new(MemoryStorage::new());
    let evm_config = OutbeEvmConfig::new_with_bridge_and_runtime_body_readers(
        chain_spec.clone(),
        bridge,
        RuntimeBodyReaders::new(body_storage),
    )
    .with_evm_signer(signer);
    let parent = Arc::new(SealedHeader::seal_slow(OutbeHeader::new(
        alloy_consensus::Header {
            number: 0,
            gas_limit: ACTIVE_PAYLOAD_BLOCK_GAS_LIMIT,
            timestamp: ACTIVE_PAYLOAD_BLOCK_TIMESTAMP - 1,
            base_fee_per_gas: Some(1_000_000_000),
            ..Default::default()
        },
    )));

    let begin = evm_config
        .build_begin_system_txs(
            1,
            chain_spec.chain().id(),
            ACTIVE_PAYLOAD_BLOCK_GAS_LIMIT,
            parent.hash(),
            &prefinal_extra_data,
            None,
            Some(proposer),
            None,
            Some(bootstrap),
        )
        .expect("active begin zone builds");
    let end = evm_config
        .build_end_system_txs(1, chain_spec.chain().id(), begin.len(), Some(proposer))
        .expect("active terminal zone builds");
    assert!(end.is_empty());
    let begin_visible_gas = begin
        .iter()
        .map(|transaction| {
            system_tx_intrinsic_gas(transaction.tx().input().as_ref())
                .expect("system transaction has valid intrinsic gas")
        })
        .sum::<u64>();
    let admission_boundary_gas = ACTIVE_PAYLOAD_BLOCK_GAS_LIMIT
        .checked_sub(begin_visible_gas)
        .expect("system zones fit inside the block gas limit");
    let candidate_gas_limit = admission_boundary_gas
        .checked_add(user_gas_over_boundary)
        .expect("test boundary delta fits u64");

    let user_tx: reth_ethereum::TransactionSigned = TxEip1559 {
        chain_id: chain_spec.chain().id(),
        nonce: 0,
        gas_limit: candidate_gas_limit,
        max_fee_per_gas: 2_000_000_000,
        max_priority_fee_per_gas: 1,
        to: TxKind::Call(address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC")),
        value: U256::ZERO,
        input: Bytes::new(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into();
    let encoded_length = user_tx.encode_2718_len();
    let recovered_user = user_tx
        .try_into_recovered()
        .expect("test user signature recovers");
    let user_sender = alloy_primitives::Address::from(*recovered_user.signer());
    let pooled_user = Arc::new(ValidPoolTransaction {
        transaction: EthPooledTransaction::new(recovered_user, encoded_length),
        transaction_id: TransactionId::new(SenderId::from(1), 0),
        propagate: true,
        timestamp: Instant::now(),
        origin: TransactionOrigin::Local,
        authority_ids: None,
    });

    let provider = active_ocomp_provider(&chain_spec, proposer, user_sender, parent.hash());
    let payload_builder = OutbePayloadBuilder::new(
        provider.clone(),
        TestPool::new(),
        evm_config.clone(),
        EthereumBuilderConfig::new().with_gas_limit(ACTIVE_PAYLOAD_BLOCK_GAS_LIMIT),
    );
    let attributes = OutbePayloadAttributes::new(
        REWARDS_ADDRESS,
        ACTIVE_PAYLOAD_BLOCK_TIMESTAMP * 1000,
        B256::repeat_byte(0x44),
        None,
        prefinal_extra_data,
        None,
        Some(proposer),
    )
    .with_execution_read_budget(ExecutionReadBudget::new());
    let payload_config = PayloadConfig::new(parent, attributes, PayloadId::new([0x07; 8]));
    let rejected = Arc::new(AtomicUsize::new(0));
    let mut payloads = Vec::with_capacity(proposal_attempts);
    for _ in 0..proposal_attempts {
        let rejected_by_builder = rejected.clone();
        let pooled_user = pooled_user.clone();
        let outcome = payload_builder
            .build_payload(
                BuildArguments::new(
                    Default::default(),
                    Default::default(),
                    None,
                    payload_config.clone(),
                    Default::default(),
                    None,
                ),
                |_| TestBestTransactions::one(pooled_user, rejected_by_builder),
            )
            .expect("every block-1 proposal attempt must retain the OST3 payload");
        payloads.push(
            outcome
                .into_payload()
                .expect("production payload builder returns a payload"),
        );
    }

    ActivePayloadCase {
        payloads,
        evm_config,
        provider,
        rejected,
    }
}

#[test]
fn bootstrap_payload_builder_ignores_nonempty_pool_and_replays_exactly() {
    let case = build_active_payload_case(0, 1);
    let payload = &case.payloads[0];
    let body = &payload.block().body().transactions;
    let layout = split_system_layout(body).expect("built payload has canonical system layout");

    assert_eq!(
        layout.begin_block_kinds().expect("begin inputs decode"),
        vec![
            SystemTxKind::CycleTick,
            SystemTxKind::RewardsGemDelivery,
            SystemTxKind::BoundaryOutcome,
            SystemTxKind::TeeBootstrap,
            SystemTxKind::OracleSlashWindow,
            SystemTxKind::HookEvents,
        ]
    );
    assert!(layout.user.is_empty());
    assert!(layout.end.is_empty());

    let executed = payload
        .executed_block()
        .expect("builder exposes the exact executed payload");
    let receipts = &executed.execution_output.result.receipts;
    assert_eq!(receipts.len(), body.len());
    assert_eq!(receipts.len(), 6);
    assert_eq!(case.rejected.load(Ordering::Relaxed), 0);

    let replay = case
        .evm_config
        .executor(StateProviderDatabase::new(&case.provider))
        .execute(executed.recovered_block.as_ref())
        .expect("validator replay succeeds");
    assert_eq!(
        replay, *executed.execution_output,
        "proposer and validator replay must produce identical receipts, gas, requests, and state"
    );
}

#[test]
fn bootstrap_payload_builder_does_not_even_evaluate_oversized_pool_candidate() {
    let case = build_active_payload_case(1, 1);
    let payload = &case.payloads[0];
    let body = &payload.block().body().transactions;
    let layout = split_system_layout(body).expect("built payload has canonical system layout");

    assert!(
        layout.user.is_empty(),
        "the candidate that consumes one unit of OSR2 reserve must not enter the block"
    );
    assert!(layout.end.is_empty());
    assert_eq!(case.rejected.load(Ordering::Relaxed), 0);

    let executed = payload
        .executed_block()
        .expect("builder exposes the exact executed payload");
    let replay = case
        .evm_config
        .executor(StateProviderDatabase::new(&case.provider))
        .execute(executed.recovered_block.as_ref())
        .expect("validator replay succeeds");
    assert_eq!(
        replay, *executed.execution_output,
        "the block that rejected the boundary+1 user must replay exactly"
    );
}

#[test]
fn bootstrap_payload_survives_repeated_block_one_proposal_attempts() {
    let case = build_active_payload_case(0, 2);
    let first_body = &case.payloads[0].block().body().transactions;
    let retry_body = &case.payloads[1].block().body().transactions;

    assert_eq!(
        retry_body, first_body,
        "a rejected block-1 candidate must not consume the mandatory OST3 payload"
    );
    assert_eq!(
        split_system_layout(retry_body)
            .expect("retry payload has canonical system layout")
            .begin_block_kinds()
            .expect("retry begin inputs decode"),
        vec![
            SystemTxKind::CycleTick,
            SystemTxKind::RewardsGemDelivery,
            SystemTxKind::BoundaryOutcome,
            SystemTxKind::TeeBootstrap,
            SystemTxKind::OracleSlashWindow,
            SystemTxKind::HookEvents,
        ]
    );
}

fn tree_service() -> (tempfile::TempDir, Arc<CompressedTreeService>) {
    let directory = tempfile::tempdir().unwrap();
    let genesis_hash = B256::repeat_byte(0x11);
    let db = CeMdbx::open(
        directory.path(),
        EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: 1,
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: outbe_compressed_entities::CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
        },
        FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: outbe_compressed_entities::sealed_root(B256::ZERO).unwrap(),
        },
    )
    .unwrap();
    let service = CompressedTreeService::new(
        db,
        CandidateCacheLimits {
            max_candidates: 4,
            max_encoded_bytes: 1_000_000,
        },
    )
    .unwrap();
    (directory, Arc::new(service))
}

#[test]
fn typed_ce_capacity_errors_survive_the_executor_boundary() {
    for expected in [
        PrecompileError::BlockCeWorkCapacityExhausted,
        PrecompileError::TransactionCeWorkLimitExceeded,
    ] {
        let expected_message = expected.to_string();
        let error = BlockExecutionError::other(expected);
        assert_eq!(
            ce_work_admission_error(&error).map(ToString::to_string),
            Some(expected_message)
        );
    }
    assert!(ce_work_admission_error(&BlockExecutionError::msg("other")).is_none());
}

#[test]
fn exact_parent_readiness_remains_typed_for_retry_without_an_alarm() {
    let readiness = BlockExecutionError::other(PrecompileError::TreeUnavailable(
        "finalized marker advanced past the payload parent".to_owned(),
    ));
    assert!(ce_local_readiness_error(&readiness));
    assert!(!ce_local_readiness_error(&BlockExecutionError::other(
        PrecompileError::Fatal("same-height parent hash mismatch".to_owned()),
    )));
}

#[test]
fn late_payload_rejection_removes_its_published_candidate() {
    let (_directory, service) = tree_service();
    let genesis_hash = B256::repeat_byte(0x11);
    let block_hash = B256::repeat_byte(0x22);
    let genesis_root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
    let provisional = service
        .open_parent(ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 0,
            block_hash: genesis_hash,
            root: genesis_root,
        })
        .unwrap()
        .prepare_seal(1, &[], &[])
        .unwrap();
    service.publish_candidate(block_hash, provisional).unwrap();

    discard_failed_payload_candidate(Some(&service), 1, block_hash).unwrap();

    assert!(service.candidate(1, block_hash).unwrap().is_none());
    assert_eq!(service.finalized_marker().unwrap().height, 0);
}
