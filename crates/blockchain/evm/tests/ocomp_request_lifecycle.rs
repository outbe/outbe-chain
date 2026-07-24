// OCOMP-TEST-ID: OCM-REQ-001

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use alloy_consensus::{Block, Header, Transaction as _};
use alloy_eips::{BlockId, BlockNumHash, BlockNumberOrTag};
use alloy_primitives::{address, keccak256, Address, Bytes, B256, U256};
use alloy_rpc_types_engine::PayloadId;
use alloy_sol_types::{SolCall, SolEvent};
use commonware_codec::Encode;
use commonware_consensus::{
    simplex::types::{Finalization, Proposal},
    types::{Epoch, Round, View},
};
use commonware_cryptography::{
    bls12381::{
        primitives::{
            ops::{aggregate, keypair, sign_message},
            variant::{MinPk, MinSig, Variant},
        },
        PrivateKey, PublicKey,
    },
    certificate::Signers,
    sha256::Digest as Sha256Digest,
    Signer,
};
use commonware_utils::Participant;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    begin_block, end_block, CandidateCacheLimits, CeMdbx, CeTopologyV1, CeWorkConfig,
    CompressedTreeService, EnvironmentIdentity, ExactParentIdentity, ExecutionScope,
    FinalizedMarker, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_consensus::{
    hybrid::HybridScheme,
    proof::{
        constants::finalize_namespace, hybrid_seed_namespace, CommitteeEntry, CommitteeSnapshot,
        HybridCertificate, VrfProof,
    },
};
use outbe_desis::{AuctionStage, DesisContract};
use outbe_evm::{
    system_tx::{split_system_layout, OcompLifecycleActivation, SystemTxInputV2, SystemTxKind},
    OutbeEvmConfig, OutbeEvmSigner, RethAccountedParentArtifactProvider,
};
use outbe_fidelity::schema::FidelityContract;
use outbe_intex::IntexContract;
use outbe_metadosis::{
    constants::{
        FORMING_PERIOD_HOURS, LOOKBACK_DELAY_HOURS, OFFERING_PERIOD_HOURS, SECONDS_PER_HOUR,
        WAITING_PERIOD_HOURS,
    },
    ocomp::schema::{poc_schema_limits, OcompRequestProfile},
    precompile::IMetadosis,
    schema::{day_type, status, MetadosisContract, WorldwideDayEntryExt},
};
use outbe_nod::NodContract;
use outbe_node::OutbePayloadBuilder;
use outbe_ocomp_protocol::{
    profile::CapacityProfileV1,
    state::{OcompJobRecordV1, OcompJobStatus},
};
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
use outbe_oracle::contract::OracleContract;
use outbe_primitives::{
    addresses::{
        COMPRESSED_ENTITIES_ADDRESS, METADOSIS_ADDRESS, REWARDS_ADDRESS, TRIBUTE_FACTORY_ADDRESS,
        VALIDATOR_SET_ADDRESS,
    },
    block::{BlockContext, BlockRuntimeContext},
    consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
    projection::ExecutionReadBudget,
    reshare_artifact::{
        decode_outbe_block_artifacts, encode_outbe_block_artifacts, CompressedEntitiesRootArtifact,
        ExecutionSummaryArtifact, OutbeBlockArtifacts,
    },
    storage::{direct::DirectStorageProvider, hashmap::HashMapStorageProvider, StorageHandle},
    time::SECONDS_PER_DAY,
    OutbeHeader, OutbePayloadAttributes, OutbePrimitives,
};
use outbe_tribute::{TributeContract, TributeData};
use outbe_validatorset::{
    committee_snapshot_key, contract::ValidatorSet, read_committee_snapshot,
    write_committee_snapshot, CommitteeSnapshot as StoredCommitteeSnapshot,
};
use rand_core::OsRng;
use reth_basic_payload_builder::{PayloadBuilder, PayloadConfig};
use reth_chainspec::{
    ChainInfo, ChainSpec, ChainSpecBuilder, ChainSpecProvider, EthereumHardfork, ForkCondition,
};
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_evm::{execute::Executor as _, ConfigureEvm, RecoveredTx};
use reth_payload_primitives::BuiltPayload as _;
use reth_primitives_traits::{Account, AlloyBlockHeader as _, Bytecode, SealedHeader};
use reth_provider::{
    test_utils::{ExtendedAccount, MockEthProvider},
    AccountReader, BlockHashReader, BlockIdReader, BlockNumReader, BytecodeReader,
    HashedPostStateProvider, ProviderResult, StateProofProvider, StateProvider, StateProviderBox,
    StateProviderFactory, StateRootProvider, StorageRootProvider,
};
use reth_revm::database::StateProviderDatabase;
use reth_transaction_pool::{noop::NoopTransactionPool, EthPooledTransaction};
use reth_trie::{
    test_utils::{state_root_prehashed, storage_root_prehashed},
    updates::TrieUpdates,
    AccountProof, ExecutionWitnessMode, HashedPostState, HashedStorage, KeccakKeyHasher,
    MultiProof, MultiProofTargets, StorageMultiProof, StorageProof, TrieInput,
};
use revm::Database;

const CHAIN_ID: u64 = 1;
const PARENT_HEIGHT: u64 = 1;
const REQUEST_HEIGHT: u64 = 2;
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const FINALIZED_EPOCH: u64 = 3;
const FINALIZED_VIEW: u64 = 100;
const PARENT_VIEW: u64 = 99;
const VRF_MATERIAL_VERSION: u64 = 5;
const VALIDATOR_OWNER: Address = address!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
const TEST_CONSENSUS_PUBLIC_KEY: [u8; 48] = [0x11; 48];

type TestPool = NoopTransactionPool<EthPooledTransaction>;
type InnerTestProvider = MockEthProvider<OutbePrimitives, ChainSpec<OutbeHeader>>;
type HashedAccountState = BTreeMap<B256, (Account, BTreeMap<B256, U256>)>;

#[derive(Clone, Debug)]
struct TestProvider {
    inner: InnerTestProvider,
    base_state: Arc<HashedAccountState>,
}

impl TestProvider {
    fn state_root_for(&self, post_state: HashedPostState) -> B256 {
        state_root_with_overlay(self.base_state.as_ref(), post_state)
    }
}

impl ChainSpecProvider for TestProvider {
    type ChainSpec = ChainSpec<OutbeHeader>;

    fn chain_spec(&self) -> Arc<Self::ChainSpec> {
        self.inner.chain_spec()
    }
}

impl BlockHashReader for TestProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        self.inner.block_hash(number)
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        self.inner.canonical_hashes_range(start, end)
    }
}

impl BlockNumReader for TestProvider {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        self.inner.chain_info()
    }

    fn best_block_number(&self) -> ProviderResult<u64> {
        self.inner.best_block_number()
    }

    fn last_block_number(&self) -> ProviderResult<u64> {
        self.inner.last_block_number()
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        self.inner.block_number(hash)
    }
}

impl BlockIdReader for TestProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }
}

impl AccountReader for TestProvider {
    fn basic_account(&self, address: &Address) -> ProviderResult<Option<Account>> {
        self.inner.basic_account(address)
    }
}

impl BytecodeReader for TestProvider {
    fn bytecode_by_hash(&self, code_hash: &B256) -> ProviderResult<Option<Bytecode>> {
        self.inner.bytecode_by_hash(code_hash)
    }
}

impl HashedPostStateProvider for TestProvider {
    fn hashed_post_state(&self, state: &revm::database::BundleState) -> HashedPostState {
        HashedPostState::from_bundle_state::<KeccakKeyHasher>(state.state())
    }
}

impl StateRootProvider for TestProvider {
    fn state_root(&self, post_state: HashedPostState) -> ProviderResult<B256> {
        Ok(self.state_root_for(post_state))
    }

    fn state_root_from_nodes(&self, input: TrieInput) -> ProviderResult<B256> {
        Ok(self.state_root_for(input.state))
    }

    fn state_root_with_updates(
        &self,
        post_state: HashedPostState,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((self.state_root_for(post_state), TrieUpdates::default()))
    }

    fn state_root_from_nodes_with_updates(
        &self,
        input: TrieInput,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((self.state_root_for(input.state), TrieUpdates::default()))
    }
}

impl StorageRootProvider for TestProvider {
    fn storage_root(&self, address: Address, post_state: HashedStorage) -> ProviderResult<B256> {
        let hashed_address = keccak256(address);
        let mut storage = self
            .base_state
            .get(&hashed_address)
            .map(|(_, storage)| storage.clone())
            .unwrap_or_default();
        apply_storage_overlay(&mut storage, post_state);
        Ok(storage_root_prehashed(storage))
    }

    fn storage_proof(
        &self,
        address: Address,
        slot: B256,
        post_state: HashedStorage,
    ) -> ProviderResult<StorageProof> {
        self.inner.storage_proof(address, slot, post_state)
    }

    fn storage_multiproof(
        &self,
        address: Address,
        slots: &[B256],
        post_state: HashedStorage,
    ) -> ProviderResult<StorageMultiProof> {
        self.inner.storage_multiproof(address, slots, post_state)
    }
}

impl StateProofProvider for TestProvider {
    fn proof(
        &self,
        input: TrieInput,
        address: Address,
        slots: &[B256],
    ) -> ProviderResult<AccountProof> {
        self.inner.proof(input, address, slots)
    }

    fn multiproof(
        &self,
        input: TrieInput,
        targets: MultiProofTargets,
    ) -> ProviderResult<MultiProof> {
        self.inner.multiproof(input, targets)
    }

    fn witness(
        &self,
        input: TrieInput,
        target: HashedPostState,
        mode: ExecutionWitnessMode,
    ) -> ProviderResult<Vec<Bytes>> {
        self.inner.witness(input, target, mode)
    }
}

impl StateProvider for TestProvider {
    fn storage(&self, account: Address, storage_key: B256) -> ProviderResult<Option<U256>> {
        self.inner.storage(account, storage_key)
    }
}

impl StateProviderFactory for TestProvider {
    fn latest(&self) -> ProviderResult<StateProviderBox> {
        Ok(Box::new(self.clone()))
    }

    fn state_by_block_number_or_tag(
        &self,
        _number_or_tag: BlockNumberOrTag,
    ) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn history_by_block_number(&self, _block: u64) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn history_by_block_hash(&self, _block: B256) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn state_by_block_hash(&self, _block: B256) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn pending(&self) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn pending_state_by_hash(&self, _block_hash: B256) -> ProviderResult<Option<StateProviderBox>> {
        self.latest().map(Some)
    }

    fn maybe_pending(&self) -> ProviderResult<Option<StateProviderBox>> {
        self.latest().map(Some)
    }

    fn state_by_block_id(&self, _block_id: BlockId) -> ProviderResult<StateProviderBox> {
        self.latest()
    }
}

struct Dkg {
    keys: Vec<PrivateKey>,
    public_keys: Vec<PublicKey>,
    vrf_group_public_key: <MinSig as Variant>::Public,
    vrf_threshold_private: commonware_cryptography::bls12381::primitives::group::Private,
}

struct PreparedParent {
    _tree_directory: tempfile::TempDir,
    tree_service: Arc<CompressedTreeService>,
    parent: Arc<SealedHeader<OutbeHeader>>,
    parent_storage: HashMap<(Address, U256), U256>,
    wwd: WorldwideDay,
    nominal: U256,
    request_time: u64,
}

#[test]
fn real_payload_builder_commits_atomic_request_after_ce_seal_without_lysis_effects() {
    let chain_spec: Arc<ChainSpec<OutbeHeader>> = ChainSpecBuilder::mainnet()
        .with_fork(EthereumHardfork::Paris, ForkCondition::Block(0))
        .build()
        .map_header(OutbeHeader::new)
        .into();
    outbe_consensus::proof::init_consensus_chain_id(CHAIN_ID);
    let signer =
        Arc::new(OutbeEvmSigner::from_secret_bytes([1u8; 32]).expect("test proposer key is valid"));
    let proposer = signer.address();
    let dkg = build_dkg();
    let snapshot = build_snapshot(&dkg);
    let prepared = prepare_parent(proposer, &snapshot);
    let metadata = finalized_parent_metadata(&dkg, &snapshot, prepared.parent.hash());
    let provider = mock_provider(&chain_spec, &prepared.parent_storage);
    provider.inner.add_block(
        prepared.parent.hash(),
        Block::new(prepared.parent.header().clone(), Default::default()),
    );
    assert_provider_snapshot(
        &provider,
        &prepared.parent_storage,
        &snapshot,
        metadata.committee_set_hash,
    );
    assert_provider_ocomp_inputs(&provider, prepared.wwd, prepared.nominal);
    let body_storage: StorageReaderHandle = Arc::new(MemoryStorage::new());
    let evm_config = OutbeEvmConfig::new_with_provider_and_runtime_body_readers(
        chain_spec.clone(),
        Arc::new(RethAccountedParentArtifactProvider::new(
            provider.inner.clone(),
            None,
        )),
        RuntimeBodyReaders::new(body_storage),
    )
    .with_evm_signer(signer)
    .with_compressed_tree_service(prepared.tree_service.clone())
    .with_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(REQUEST_HEIGHT));
    let phase1 = evm_config
        .build_signed_phase1_tx(
            REQUEST_HEIGHT,
            CHAIN_ID,
            prepared.parent.hash(),
            Some(metadata.clone()),
            Some(proposer),
        )
        .unwrap()
        .expect("block 2 has a Phase 1 transaction");
    let decoded_metadata = match SystemTxInputV2::decode(phase1.tx().input().as_ref()).unwrap() {
        SystemTxInputV2::CertifiedParentAccounting { metadata } => metadata,
        other => panic!("expected Phase 1 metadata, got {other:?}"),
    };
    assert_eq!(decoded_metadata, metadata);
    outbe_consensus::proof::verify_v2_proof(
        &decoded_metadata,
        &snapshot,
        decoded_metadata.proof.as_ref(),
        prepared.parent.hash(),
    )
    .expect("signed Phase 1 transaction preserves the valid proof");
    let payload_builder = OutbePayloadBuilder::new(
        provider.clone(),
        TestPool::new(),
        evm_config.clone(),
        EthereumBuilderConfig::new().with_gas_limit(BLOCK_GAS_LIMIT),
    );
    let attributes = OutbePayloadAttributes::new(
        REWARDS_ADDRESS,
        prepared.request_time * 1_000,
        B256::repeat_byte(0x44),
        Some(B256::repeat_byte(0x45)),
        Bytes::new(),
        Some(metadata),
        Some(proposer),
    )
    .with_execution_read_budget(ExecutionReadBudget::new());
    let payload = payload_builder
        .build_empty_payload(PayloadConfig::new(
            prepared.parent.clone(),
            attributes,
            PayloadId::new([0x08; 8]),
        ))
        .expect("production payload builder must create the request block");

    let body = &payload.block().body().transactions;
    let layout = split_system_layout(body).expect("request block has canonical system layout");
    assert_eq!(
        layout
            .begin_block_kinds()
            .expect("begin-zone inputs decode"),
        vec![
            SystemTxKind::CertifiedParentAccounting,
            SystemTxKind::LateFinalizeCredits,
            SystemTxKind::OcompLifecycleBegin,
            SystemTxKind::CycleTick,
            SystemTxKind::OracleSlashWindow,
            SystemTxKind::HookEvents,
        ]
    );
    assert!(layout.user.is_empty());
    assert_eq!(
        layout.end_block_kinds().expect("end-zone inputs decode"),
        vec![SystemTxKind::OcompTerminalRequest]
    );

    let executed = payload
        .executed_block()
        .expect("builder exposes its exact production execution");
    let receipts = &executed.execution_output.result.receipts;
    assert_eq!(receipts.len(), body.len());
    let terminal_receipt = receipts.last().expect("terminal request receipt");
    assert!(terminal_receipt.success);
    let requested = terminal_receipt
        .logs
        .iter()
        .find_map(|log| IMetadosis::OffchainJobRequested::decode_log(log).ok())
        .expect("terminal receipt exposes OffchainJobRequested");
    assert_eq!(requested.data.wwd, prepared.wwd.value());
    assert_eq!(requested.data.pendingNonce, 0);
    assert_eq!(requested.data.attempt, 0);

    let replay = evm_config
        .executor(StateProviderDatabase::new(&provider))
        .execute(executed.recovered_block.as_ref())
        .expect("validator replay succeeds through the production executor");
    assert_eq!(
        replay, *executed.execution_output,
        "proposer and validator must agree on receipts, requests and post-state"
    );

    let artifacts = decode_outbe_block_artifacts(payload.block().header().extra_data().as_ref())
        .expect("request header artifacts decode");
    let ce_artifact = artifacts
        .compressed_entities_root
        .expect("request header carries the completed CE seal");
    assert_eq!(
        ce_artifact.commitment_scheme_version,
        ACTIVE_COMMITMENT_SCHEME
    );
    let exact_post_state = provider.hashed_post_state(&executed.execution_output.state);
    let expected_state_root = provider.state_root_for(exact_post_state.clone());
    assert_eq!(
        payload.block().header().state_root(),
        expected_state_root,
        "header state root must be derived from the real execution BundleState"
    );
    let mutated_state_root = provider.state_root_for(mutate_one_storage_value(exact_post_state));
    assert_ne!(
        mutated_state_root, expected_state_root,
        "the independent root oracle must be sensitive to a real post-state mutation"
    );
    assert_ne!(
        payload.block().header().state_root(),
        prepared.parent.header().state_root(),
        "request execution must produce an observable state-root transition"
    );
    prepared
        .tree_service
        .apply_finalized(REQUEST_HEIGHT, payload.block().hash(), ce_artifact.r_sealed)
        .expect("production CE finalizer applies the exact built candidate");
    assert_eq!(
        prepared.tree_service.finalized_marker().unwrap().new_root,
        ce_artifact.r_sealed
    );

    let mut post_state = HashMapStorageProvider::new(CHAIN_ID);
    post_state.storage = prepared.parent_storage;
    apply_bundle(&mut post_state, executed.execution_output.state.state());
    StorageHandle::enter(&mut post_state, |storage| {
        let public_call = IMetadosis::getOffchainJobCall {
            intentId: requested.data.intentId,
        };
        let encoded = outbe_metadosis::precompile::dispatch(
            storage.clone(),
            &public_call.abi_encode(),
            proposer,
            U256::ZERO,
        )
        .unwrap();
        let public_bytes = IMetadosis::getOffchainJobCall::abi_decode_returns(&encoded).unwrap();
        let record =
            OcompJobRecordV1::decode_canonical(public_bytes.as_ref(), &poc_schema_limits())
                .unwrap();
        assert_eq!(record.status, OcompJobStatus::OffchainPending);
        assert_eq!(record.intent.wwd, prepared.wwd.value());
        assert_eq!(record.intent.pending_nonce, 0);
        assert_eq!(record.intent.authenticated_day_count, 1);
        assert_eq!(record.intent.authenticated_day_nominal, prepared.nominal);
        assert_eq!(record.intent.ce_sealed_root, ce_artifact.r_sealed);
        assert_eq!(record.intent.deadline_height, REQUEST_HEIGHT + 64);
        assert_eq!(
            record
                .intent
                .activation_preconditions
                .metadosis
                .expected_status,
            outbe_ocomp_protocol::intent::MetadosisExpectedStatus::OffchainPending
        );
        assert_eq!(
            record.intent.frozen_metadosis_values.day_limit,
            record
                .intent
                .frozen_metadosis_values
                .lysis_budget
                .checked_add(record.intent.frozen_metadosis_values.auction_base)
                .unwrap()
        );
        assert!(!record.intent.frozen_metadosis_values.lysis_budget.is_zero());
        assert_ne!(
            record
                .intent
                .frozen_metadosis_values
                .request_budget_split_receipt_hash,
            B256::ZERO
        );
        assert_eq!(
            DesisContract::new(storage.clone())
                .auction_stage
                .read(&prepared.wwd.value())
                .unwrap(),
            AuctionStage::Briefed as u8
        );
        assert_eq!(
            DesisContract::new(storage.clone())
                .pending_supply_promis
                .read(&prepared.wwd.value())
                .unwrap(),
            record.intent.frozen_metadosis_values.auction_base
        );

        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert_eq!(
            IntexContract::new(storage.clone())
                .total_series
                .read()
                .unwrap(),
            0
        );
        assert_eq!(
            IntexContract::new(storage.clone())
                .contributor_count
                .read(&prepared.wwd.value())
                .unwrap(),
            0
        );
        let tribute = TributeContract::new(storage);
        assert_eq!(tribute.total_supply().unwrap(), 1);
        let totals = tribute.get_day_totals(prepared.wwd).unwrap();
        assert_eq!(totals.tribute_count, 1);
        assert_eq!(totals.tribute_nominal_amount, prepared.nominal);
    });

    assert_ne!(requested.data.intentId, B256::ZERO);
    assert_eq!(
        terminal_receipt
            .logs
            .iter()
            .filter(|log| {
                log.address == METADOSIS_ADDRESS
                    && IMetadosis::OffchainJobRequested::decode_log(log).is_ok()
            })
            .count(),
        1
    );
    assert!(terminal_receipt
        .logs
        .iter()
        .all(|log| log.address != TRIBUTE_FACTORY_ADDRESS));
}

fn prepare_parent(proposer: Address, snapshot: &StoredCommitteeSnapshot) -> PreparedParent {
    let directory = tempfile::tempdir().unwrap();
    let genesis_hash = B256::repeat_byte(0x11);
    let db = CeMdbx::open(
        directory.path(),
        EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: CHAIN_ID,
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
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
    let tree_service = Arc::new(
        CompressedTreeService::new(
            db,
            CandidateCacheLimits {
                max_candidates: 4,
                max_encoded_bytes: 1_000_000,
            },
        )
        .unwrap(),
    );
    let marker = tree_service.finalized_marker().unwrap();
    let parent_tree = tree_service
        .open_parent(ExactParentIdentity {
            commitment_scheme_version: marker.commitment_scheme_version,
            block_number: marker.height,
            block_hash: marker.block_hash,
            root: marker.new_root,
        })
        .unwrap();
    let scope = ExecutionScope::with_parent_tree(parent_tree, CeWorkConfig::new(0, 0, u64::MAX));
    let mut seed = HashMapStorageProvider::new(CHAIN_ID);
    seed.set_block_number(PARENT_HEIGHT);
    let wwd = WorldwideDay::new(2026_0710);
    let parent_time = wwd.start_timestamp();
    let request_time = parent_time + SECONDS_PER_DAY;
    let nominal = U256::from(1_000);
    let owner = address!("7300000000000000000000000000000000000073");
    let mut profile = request_profile();
    profile.chain_id = CHAIN_ID;

    let seal = StorageHandle::enter(&mut seed, |storage| {
        seed_ce_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();

        let mut validators = ValidatorSet::new(storage.clone());
        validators.config_owner.write(VALIDATOR_OWNER).unwrap();
        validators.config_max_validators.write(128).unwrap();
        validators.config_epoch_length_blocks.write(60).unwrap();
        validators.config_is_initialized.write(true).unwrap();
        for entry in &snapshot.committee {
            validators
                .register_validator(VALIDATOR_OWNER, entry.address, &entry.consensus_pubkey)
                .unwrap();
        }
        validators
            .register_validator(VALIDATOR_OWNER, proposer, &TEST_CONSENSUS_PUBLIC_KEY)
            .unwrap();
        validators
            .activate_reshared_set(&[proposer], B256::ZERO)
            .unwrap();
        write_committee_snapshot(storage.clone(), FINALIZED_EPOCH, snapshot).unwrap();

        FidelityContract::new(storage.clone())
            .initialize_fresh_ocomp_profile()
            .unwrap();
        let mut oracle = OracleContract::new(storage.clone());
        let mut oracle_genesis = outbe_oracle::logic::OracleGenesisConfig::default_config();
        oracle_genesis.initial_rates.push((
            "COEN".to_owned(),
            "0xUSD".to_owned(),
            U256::from(2_000_000_000_000_000_000u128),
        ));
        oracle_genesis.settlement_currencies.push((
            840,
            "0xUSD".to_owned(),
            "COEN".to_owned(),
            "0xUSD".to_owned(),
        ));
        outbe_oracle::logic::init_from_genesis(&mut oracle, &oracle_genesis).unwrap();
        outbe_oracle::api::initialize_fresh_ocomp_profile(storage.clone()).unwrap();

        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .initialize_ocomp_request_profile(&profile, &poc_schema_limits())
            .unwrap();
        metadosis
            .create_worldwide_day(
                wwd,
                wwd.start_timestamp(),
                LOOKBACK_DELAY_HOURS,
                OFFERING_PERIOD_HOURS,
            )
            .unwrap();
        metadosis.add_active_wwd(wwd).unwrap();
        let scheduled = wwd.start_timestamp()
            + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR
            + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR
            + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR
            + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;
        assert_eq!(
            metadosis.update_wwd_status(wwd, scheduled).unwrap(),
            status::READY
        );
        metadosis.set_wwd_day_type(wwd, day_type::GREEN).unwrap();
        metadosis.set_wwd_vwap(wwd, U256::from(2)).unwrap();
        metadosis.set_metadosis_limit(wwd, U256::from(100)).unwrap();
        let mut tribute = TributeContract::new(storage.clone());
        tribute.initialize_fresh_ocomp_profile().unwrap();
        tribute.unseal_day(wwd).unwrap();
        tribute
            .issue(
                &scope,
                &EmptyParent,
                &TributeData {
                    tribute_id: NodContract::generate_nod_id(owner, wwd).unwrap(),
                    owner,
                    worldwide_day: wwd,
                    issuance_amount_minor: nominal,
                    issuance_currency: 840,
                    nominal_amount_minor: nominal,
                    reference_currency: 840,
                    exclude_from_intex_issuance: false,
                    tribute_price_minor: U256::from(2),
                },
            )
            .unwrap();
        tribute.seal_day(wwd).unwrap();

        let parent_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(PARENT_HEIGHT, parent_time, CHAIN_ID),
            storage.clone(),
        );
        outbe_rewards::runtime::ensure_genesis_anchor(&parent_ctx).unwrap();
        outbe_cycle::runtime::dispatch_triggers(&parent_ctx, &scope, &EmptyParent).unwrap();
        end_block(storage, &scope).unwrap()
    });

    let parent_extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        }),
        consensus_header_artifact: None,
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        compressed_entities_root: Some(CompressedEntitiesRootArtifact {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            r_sealed: seal.new_root,
        }),
    })
    .unwrap();
    let parent_state_root = state_root_prehashed(hashed_marker_state(&seed.storage));
    let parent = Arc::new(SealedHeader::seal_slow(OutbeHeader::new(Header {
        parent_hash: genesis_hash,
        beneficiary: REWARDS_ADDRESS,
        state_root: parent_state_root,
        number: PARENT_HEIGHT,
        gas_limit: BLOCK_GAS_LIMIT,
        timestamp: parent_time,
        base_fee_per_gas: Some(1_000_000_000),
        extra_data: parent_extra_data,
        ..Default::default()
    })));
    tree_service
        .publish_candidate(parent.hash(), seal.staged_tree_batch)
        .unwrap();
    tree_service
        .apply_finalized(PARENT_HEIGHT, parent.hash(), seal.new_root)
        .unwrap();

    PreparedParent {
        _tree_directory: directory,
        tree_service,
        parent,
        parent_storage: seed.storage,
        wwd,
        nominal,
        request_time,
    }
}

#[derive(Debug)]
struct EmptyParent;

impl outbe_compressed_entities::ParentBodySource for EmptyParent {
    fn get(
        &self,
        _entity: outbe_compressed_entities::EntityRef,
    ) -> Result<
        Option<outbe_compressed_entities::StoredBody>,
        outbe_compressed_entities::ParentBodySourceError,
    > {
        Ok(None)
    }

    fn list(
        &self,
        _query: outbe_compressed_entities::QueryRef,
        _request: outbe_compressed_entities::IdPageRequest,
    ) -> Result<outbe_compressed_entities::IdPage, outbe_compressed_entities::ParentBodySourceError>
    {
        Ok(outbe_compressed_entities::IdPage {
            ids: Vec::new(),
            next_after: None,
        })
    }
}

fn mock_provider(
    chain_spec: &Arc<ChainSpec<OutbeHeader>>,
    storage: &HashMap<(Address, U256), U256>,
) -> TestProvider {
    let inner =
        MockEthProvider::<OutbePrimitives>::new().with_chain_spec(chain_spec.as_ref().clone());
    let mut accounts: BTreeMap<Address, Vec<(B256, U256)>> = BTreeMap::new();
    for ((account, slot), value) in storage {
        accounts
            .entry(*account)
            .or_default()
            .push((B256::from(slot.to_be_bytes::<32>()), *value));
    }
    for (account, account_storage) in accounts {
        inner.add_account(
            account,
            ExtendedAccount::new(0, U256::ZERO)
                .with_bytecode(Bytes::from_static(&[0xef]))
                .extend_storage(account_storage),
        );
    }
    TestProvider {
        inner,
        base_state: Arc::new(hashed_marker_state(storage)),
    }
}

fn hashed_marker_state(storage: &HashMap<(Address, U256), U256>) -> HashedAccountState {
    let marker_code_hash = keccak256([0xef]);
    let mut accounts = HashedAccountState::new();
    for ((address, slot), value) in storage {
        let (_, account_storage) = accounts.entry(keccak256(address)).or_insert_with(|| {
            (
                Account {
                    nonce: 0,
                    balance: U256::ZERO,
                    bytecode_hash: Some(marker_code_hash),
                },
                BTreeMap::new(),
            )
        });
        if !value.is_zero() {
            account_storage.insert(keccak256(B256::from(slot.to_be_bytes::<32>())), *value);
        }
    }
    accounts
}

fn apply_storage_overlay(storage: &mut BTreeMap<B256, U256>, post_state: HashedStorage) {
    if post_state.wiped {
        storage.clear();
    }
    for (slot, value) in post_state.storage {
        if value.is_zero() {
            storage.remove(&slot);
        } else {
            storage.insert(slot, value);
        }
    }
}

fn state_root_with_overlay(base_state: &HashedAccountState, post_state: HashedPostState) -> B256 {
    let mut state = base_state.clone();
    for (address, account) in post_state.accounts {
        match account {
            Some(account) => {
                let storage = state
                    .remove(&address)
                    .map(|(_, storage)| storage)
                    .unwrap_or_default();
                state.insert(address, (account, storage));
            }
            None => {
                state.remove(&address);
            }
        }
    }
    for (address, storage_overlay) in post_state.storages {
        let Some((_, storage)) = state.get_mut(&address) else {
            assert!(
                storage_overlay.wiped && storage_overlay.storage.values().all(U256::is_zero),
                "post-state storage exists without a live account"
            );
            continue;
        };
        apply_storage_overlay(storage, storage_overlay);
    }
    state_root_prehashed(state)
}

fn mutate_one_storage_value(mut post_state: HashedPostState) -> HashedPostState {
    let value = post_state
        .storages
        .values_mut()
        .flat_map(|storage| storage.storage.values_mut())
        .next()
        .expect("request execution changes at least one storage value");
    *value = if *value == U256::MAX {
        value.checked_sub(U256::from(1)).unwrap()
    } else {
        value.checked_add(U256::from(1)).unwrap()
    };
    post_state
}

fn apply_bundle(
    target: &mut HashMapStorageProvider,
    state: &revm::primitives::AddressMap<revm::database::BundleAccount>,
) {
    for (address, account) in state {
        for (slot, value) in &account.storage {
            target
                .storage
                .insert((*address, *slot), value.present_value());
        }
    }
}

fn assert_provider_snapshot(
    provider: &TestProvider,
    parent_storage: &HashMap<(Address, U256), U256>,
    expected: &StoredCommitteeSnapshot,
    committee_set_hash: B256,
) {
    let mut database = StateProviderDatabase::new(provider);
    let mut mirror = HashMapStorageProvider::new(CHAIN_ID);
    for ((address, slot), expected_value) in parent_storage
        .iter()
        .filter(|((address, _), _)| *address == VALIDATOR_SET_ADDRESS)
    {
        let actual = database.storage(*address, *slot).unwrap();
        assert_eq!(actual, *expected_value);
        mirror.storage.insert((*address, *slot), actual);
    }
    StorageHandle::enter(&mut mirror, |storage| {
        assert_eq!(
            read_committee_snapshot(
                storage,
                committee_snapshot_key(FINALIZED_EPOCH, committee_set_hash),
            )
            .unwrap(),
            Some(expected.clone())
        );
    });

    let context = BlockContext::empty_for_tests(REQUEST_HEIGHT, 0, CHAIN_ID);
    let mut state = reth_revm::db::State::builder()
        .with_database(database)
        .with_bundle_update()
        .build();
    let mut direct = DirectStorageProvider::new(&mut state, context);
    let storage = StorageHandle::new(&mut direct);
    assert_eq!(
        read_committee_snapshot(
            storage,
            committee_snapshot_key(FINALIZED_EPOCH, committee_set_hash),
        )
        .unwrap(),
        Some(expected.clone())
    );
}

fn assert_provider_ocomp_inputs(
    provider: &TestProvider,
    wwd: WorldwideDay,
    expected_nominal: U256,
) {
    let database = StateProviderDatabase::new(provider);
    let context = BlockContext::empty_for_tests(REQUEST_HEIGHT, 0, CHAIN_ID);
    let mut state = reth_revm::db::State::builder()
        .with_database(database)
        .with_bundle_update()
        .build();
    let mut direct = DirectStorageProvider::new(&mut state, context);
    let storage = StorageHandle::new(&mut direct);
    let metadosis = MetadosisContract::new(storage.clone());
    assert_eq!(
        metadosis
            .read_ocomp_request_profile(&poc_schema_limits())
            .unwrap(),
        Some(request_profile())
    );
    assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::READY);
    assert_eq!(metadosis.active_wwd.read_all().unwrap(), vec![wwd]);
    assert_eq!(metadosis.get_wwd_day_type(wwd).unwrap(), day_type::GREEN);
    assert_eq!(
        metadosis
            .worldwide_days
            .entry(wwd)
            .metadosis_limit_amount()
            .read()
            .unwrap(),
        U256::from(100)
    );
    let totals = TributeContract::new(storage).get_day_totals(wwd).unwrap();
    assert_eq!(totals.tribute_count, 1);
    assert_eq!(totals.tribute_nominal_amount, expected_nominal);
}

fn seed_ce_genesis(storage: &StorageHandle<'_>) {
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(3))
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
}

fn request_profile() -> OcompRequestProfile {
    OcompRequestProfile {
        chain_id: CHAIN_ID,
        genesis_hash: B256::repeat_byte(0x11),
        fork_id: B256::repeat_byte(0x21),
        protocol_bundle_hash: B256::repeat_byte(0x41),
        correctness_profile_id: B256::repeat_byte(0x24),
        capacity_profile: CapacityProfileV1 {
            profile_id: B256::repeat_byte(0x22),
            max_tributes_per_work_shard: 256,
            max_workers_per_domain: 4,
            max_pending_jobs: 1,
            max_intents_per_block: 1,
            max_activations_per_block: 1,
            max_ready_inspections_per_block: 1,
            max_expirations_per_block: 1,
            retry_backoff_blocks: 1,
            max_terminal_job_records: 365,
            max_reference_currencies: 256,
            max_fidelity_cohorts_per_owner: 64,
            max_oracle_wwd_pair_entries: 256,
            max_active_scurve_entries: 256,
            result_deadline_blocks: 64,
            source_retention_after_terminal_blocks: 64,
            generated_limits_manifest_hash: B256::repeat_byte(0x23),
        },
        source_availability_policy_id: B256::repeat_byte(0x35),
        result_committee_snapshot_hash: B256::repeat_byte(0x36),
    }
}

fn build_dkg() -> Dkg {
    let keys = (0..4)
        .map(|index| PrivateKey::from_seed(index + 1))
        .collect::<Vec<_>>();
    let public_keys = keys
        .iter()
        .cloned()
        .map(PublicKey::from)
        .collect::<Vec<_>>();
    let mut rng = OsRng;
    let (vrf_threshold_private, vrf_group_public_key) = keypair::<_, MinSig>(&mut rng);
    Dkg {
        keys,
        public_keys,
        vrf_group_public_key,
        vrf_threshold_private,
    }
}

fn build_snapshot(dkg: &Dkg) -> CommitteeSnapshot {
    CommitteeSnapshot {
        committee: dkg
            .public_keys
            .iter()
            .enumerate()
            .map(|(index, public_key)| {
                let mut consensus_pubkey = [0u8; 48];
                consensus_pubkey.copy_from_slice(public_key.encode().as_ref());
                CommitteeEntry {
                    address: Address::with_last_byte((index + 1) as u8),
                    consensus_pubkey,
                }
            })
            .collect(),
        vrf_material_version: VRF_MATERIAL_VERSION,
        vrf_group_public_key_bytes: dkg.vrf_group_public_key.encode().to_vec(),
        vrf_public_polynomial_hash: B256::ZERO,
    }
}

fn finalized_parent_metadata(
    dkg: &Dkg,
    snapshot: &CommitteeSnapshot,
    parent_hash: B256,
) -> CertifiedParentAccountingMetadata {
    let round = Round::new(Epoch::new(FINALIZED_EPOCH), View::new(FINALIZED_VIEW));
    let proposal =
        Proposal::<Sha256Digest>::new(round, View::new(PARENT_VIEW), Sha256Digest(parent_hash.0));
    let vote_message = proposal.encode().to_vec();
    let seed_message = round.encode().to_vec();
    let committee_set = commonware_utils::ordered::Set::from_iter_dedup(
        dkg.keys.iter().map(|key| key.public_key()),
    );
    let namespace = finalize_namespace(&committee_set);
    let signatures = dkg
        .keys
        .iter()
        .map(|key| key.sign(&namespace, &vote_message))
        .collect::<Vec<_>>();
    let certificate = HybridCertificate::<MinSig> {
        signers: Signers::from(
            dkg.keys.len(),
            (0..u32::try_from(dkg.keys.len()).unwrap()).map(Participant::new),
        ),
        bls_aggregated_vote: aggregate::combine_signatures::<MinPk, _>(
            signatures.iter().map(|signature| signature.as_ref()),
        ),
        vrf_proof: Some(VrfProof::<MinSig> {
            material_version: VRF_MATERIAL_VERSION,
            threshold_signature: sign_message::<MinSig>(
                &dkg.vrf_threshold_private,
                &hybrid_seed_namespace(),
                &seed_message,
            ),
        }),
    };
    let proof = Finalization::<HybridScheme<MinSig>, Sha256Digest> {
        proposal,
        certificate,
    }
    .encode()
    .to_vec();
    let committee_set_hash =
        outbe_consensus::proof::committee_set_hash_v2(FINALIZED_EPOCH, snapshot);
    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number: PARENT_HEIGHT,
        finalized_block_hash: parent_hash,
        finalized_epoch: FINALIZED_EPOCH,
        finalized_view: FINALIZED_VIEW,
        parent_view: PARENT_VIEW,
        ordered_committee: snapshot
            .committee
            .iter()
            .map(|entry| entry.address)
            .collect(),
        signer_bitmap: vec![1; snapshot.committee.len()],
        proof: Bytes::from(proof),
        committee_set_hash,
        vrf_material_version: VRF_MATERIAL_VERSION,
        vrf_group_public_key_hash: keccak256(&snapshot.vrf_group_public_key_bytes),
        proof_kind: ParentParticipationProof::Finalization,
        missed_proposers: Vec::new(),
    };
    outbe_consensus::proof::verify_v2_proof(
        &metadata,
        snapshot,
        metadata.proof.as_ref(),
        parent_hash,
    )
    .expect("real finalized-parent proof fixture verifies before execution");
    metadata
}
