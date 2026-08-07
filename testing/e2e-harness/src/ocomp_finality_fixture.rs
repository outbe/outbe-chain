//! Real q=3/4 Commonware finality and Ethereum-MPT fixture for OCOMP process tests.
//!
//! This module does not provide an accepting verifier or inject a trusted
//! result. It constructs the same canonical proof bytes consumed by the
//! production verifier, allowing process-boundary tests to derive `JobIntent`
//! exclusively from authenticated node responses.

use std::collections::BTreeMap;

use alloy_consensus::Header;
use alloy_eips::{BlockNumHash, BlockNumberOrTag};
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::Encodable as _;
use alloy_trie::{proof::ProofRetainer, HashBuilder, Nibbles, TrieAccount, KECCAK_EMPTY};
use commonware_codec::Encode as _;
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
    Signer as _,
};
use commonware_utils::{ordered::Set, Participant};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::TributeBodyV1;
use outbe_consensus::{
    block::ConsensusBlock,
    hybrid::HybridScheme,
    proof::{
        constants::finalize_namespace, hybrid_seed_namespace, CommitteeEntry, CommitteeSnapshot,
        HybridCertificate, VrfProof,
    },
};
use outbe_fidelity::{MAX_LEAGUE, MIN_LEAGUE};
use outbe_metadosis::proof_layout::OCOMP_JOB_RECORDS_BASE_SLOT;
use outbe_node::ocomp::finality::{PublicAccountProofV1, PublicBlockViewV1, PublicStorageProofV1};
use outbe_ocomp_protocol::{
    common::{BoundedBytes, ProofBytes},
    intent::{
        intent_storage_key, CertifiedParentAccountingMetadataV2, FinalizedIntentProofV1,
        JobIntentV1, ParentProofKind,
    },
    league_snapshot::league_snapshot_slot,
    opening::OpeningSubjectsV1,
    state::{OcompJobRecordV1, OcompJobStatus},
    SchemaLimits,
};
use outbe_oracle::{oracle_count_slot_plan_v1, oracle_opening_slot_plan_v1};
use outbe_primitives::{
    addresses::{METADOSIS_ADDRESS, ORACLE_ADDRESS, VALIDATOR_SET_ADDRESS},
    header::OutbeHeader,
    storage::types::StorageKey as _,
    OutbeBlock,
};
use rand::{rngs::StdRng, SeedableRng as _};
use reth_chainspec::ChainInfo;
use reth_primitives_traits::{Account, Bytecode, SealedBlock};
use reth_storage_api::{
    errors::provider::ProviderResult, AccountReader, BlockHashReader, BlockIdReader,
    BlockNumReader, BytecodeReader, HashedPostStateProvider, StateProofProvider, StateProvider,
    StateProviderBox, StateProviderFactory, StateRootProvider, StorageRootProvider,
};
use reth_trie::{
    updates::TrieUpdates, AccountProof, ExecutionWitnessMode, HashedPostState, HashedStorage,
    KeccakKeyHasher, MultiProof, MultiProofTargets, StorageMultiProof, StorageProof, TrieInput,
};

const FINALIZED_EPOCH: u64 = 2;
const FINALIZED_VIEW: u64 = 3;
const PARENT_VIEW: u64 = 2;
const VRF_MATERIAL_VERSION: u64 = 5;
const SIGNER_INDICES: [u32; 3] = [0, 1, 2];

/// Authenticated inputs generated for one exact process-test intent.
pub struct FinalizedIntentProofFixture {
    pub intent: JobIntentV1,
    pub intent_id: B256,
    pub job_id: B256,
    pub proof: FinalizedIntentProofV1,
    pub state_root: B256,
    pub header_hash: B256,
    pub block: ConsensusBlock,
    pub canonical_history: CanonicalHistoryFixture,
    pub public_exact_block: PublicExactBlockFixtureV1,
}

#[derive(Clone)]
pub struct PublicExactBlockFixtureV1 {
    pub finalization_bytes: Vec<u8>,
    pub block_bytes: Vec<u8>,
    pub block_view: PublicBlockViewV1,
    pub intent_id: B256,
    pub canonical_job_record: Vec<u8>,
    pub account_proofs: BTreeMap<Address, PublicAccountProofV1>,
}

/// Finalized JobIntent plus an exact historical state provider for the
/// production Fidelity/Oracle opening builder.
pub struct FinalizedLysisInputFixture {
    pub finalized: FinalizedIntentProofFixture,
    pub opening_provider: LysisOpeningProvider,
    pub subjects: OpeningSubjectsV1,
}

#[derive(Clone)]
pub struct LysisOpeningProvider {
    state: LysisOpeningState,
    block_number: u64,
    block_hash: B256,
}

#[derive(Clone)]
struct LysisOpeningState {
    state_root: B256,
    accounts: BTreeMap<Address, Account>,
    storage: BTreeMap<(Address, B256), U256>,
    proofs: BTreeMap<Address, AccountProof>,
}

/// Minimal exact canonical-history provider used by the production containment
/// adapter in process tests.
#[derive(Clone)]
pub struct CanonicalHistoryFixture {
    hashes: BTreeMap<u64, B256>,
    finalized: BlockNumHash,
}

impl CanonicalHistoryFixture {
    #[must_use]
    pub fn new(block_number: u64, block_hash: B256) -> Self {
        Self {
            hashes: BTreeMap::from([(block_number, block_hash)]),
            finalized: BlockNumHash::new(block_number, block_hash),
        }
    }

    #[must_use]
    pub fn with_canonical_block(mut self, block_number: u64, block_hash: B256) -> Self {
        self.hashes.insert(block_number, block_hash);
        if block_number > self.finalized.number {
            self.finalized = BlockNumHash::new(block_number, block_hash);
        }
        self
    }
}

impl BlockHashReader for CanonicalHistoryFixture {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok(self.hashes.get(&number).copied())
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok((start..end)
            .filter_map(|number| self.hashes.get(&number).copied())
            .collect())
    }
}

impl BlockNumReader for CanonicalHistoryFixture {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        Ok(ChainInfo {
            best_hash: self.finalized.hash,
            best_number: self.finalized.number,
        })
    }

    fn best_block_number(&self) -> ProviderResult<u64> {
        Ok(self.finalized.number)
    }

    fn last_block_number(&self) -> ProviderResult<u64> {
        Ok(self.finalized.number)
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        Ok(self
            .hashes
            .iter()
            .find_map(|(number, candidate)| (*candidate == hash).then_some(*number)))
    }
}

impl BlockIdReader for CanonicalHistoryFixture {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }
}

impl AccountReader for LysisOpeningState {
    fn basic_account(&self, address: &Address) -> ProviderResult<Option<Account>> {
        Ok(self.accounts.get(address).copied())
    }
}

impl BlockHashReader for LysisOpeningState {
    fn block_hash(&self, _number: u64) -> ProviderResult<Option<B256>> {
        Ok(None)
    }

    fn canonical_hashes_range(&self, _start: u64, _end: u64) -> ProviderResult<Vec<B256>> {
        Ok(Vec::new())
    }
}

impl BytecodeReader for LysisOpeningState {
    fn bytecode_by_hash(&self, _code_hash: &B256) -> ProviderResult<Option<Bytecode>> {
        Ok(None)
    }
}

impl StateRootProvider for LysisOpeningState {
    fn state_root(&self, _hashed_state: HashedPostState) -> ProviderResult<B256> {
        Ok(self.state_root)
    }

    fn state_root_from_nodes(&self, _input: TrieInput) -> ProviderResult<B256> {
        Ok(self.state_root)
    }

    fn state_root_with_updates(
        &self,
        _hashed_state: HashedPostState,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((self.state_root, TrieUpdates::default()))
    }

    fn state_root_from_nodes_with_updates(
        &self,
        _input: TrieInput,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((self.state_root, TrieUpdates::default()))
    }
}

impl StorageRootProvider for LysisOpeningState {
    fn storage_root(
        &self,
        address: Address,
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<B256> {
        Ok(self
            .proofs
            .get(&address)
            .map_or(B256::ZERO, |proof| proof.storage_root))
    }

    fn storage_proof(
        &self,
        address: Address,
        slot: B256,
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<StorageProof> {
        Ok(self
            .proofs
            .get(&address)
            .and_then(|proof| proof.storage_proofs.iter().find(|proof| proof.key == slot))
            .cloned()
            .unwrap_or_else(|| StorageProof::new(slot)))
    }

    fn storage_multiproof(
        &self,
        _address: Address,
        _slots: &[B256],
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<StorageMultiProof> {
        Ok(StorageMultiProof::empty())
    }
}

impl StateProofProvider for LysisOpeningState {
    fn proof(
        &self,
        _input: TrieInput,
        address: Address,
        slots: &[B256],
    ) -> ProviderResult<AccountProof> {
        let mut proof = self
            .proofs
            .get(&address)
            .cloned()
            .unwrap_or_else(|| AccountProof::new(address));
        proof.storage_proofs = slots
            .iter()
            .map(|slot| {
                proof
                    .storage_proofs
                    .iter()
                    .find(|storage| storage.key == *slot)
                    .cloned()
                    .unwrap_or_else(|| StorageProof::new(*slot))
            })
            .collect();
        Ok(proof)
    }

    fn multiproof(
        &self,
        _input: TrieInput,
        _targets: MultiProofTargets,
    ) -> ProviderResult<MultiProof> {
        Ok(MultiProof::default())
    }

    fn witness(
        &self,
        _input: TrieInput,
        _target: HashedPostState,
        _mode: ExecutionWitnessMode,
    ) -> ProviderResult<Vec<Bytes>> {
        Ok(Vec::new())
    }
}

impl HashedPostStateProvider for LysisOpeningState {
    fn hashed_post_state(&self, bundle_state: &revm::database::BundleState) -> HashedPostState {
        HashedPostState::from_bundle_state::<KeccakKeyHasher>(bundle_state.state())
    }
}

impl StateProvider for LysisOpeningState {
    fn storage(&self, account: Address, storage_key: B256) -> ProviderResult<Option<U256>> {
        Ok(self.storage.get(&(account, storage_key)).copied())
    }
}

impl BlockHashReader for LysisOpeningProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok((number == self.block_number).then_some(self.block_hash))
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok((start..end)
            .filter_map(|number| (number == self.block_number).then_some(self.block_hash))
            .collect())
    }
}

impl BlockNumReader for LysisOpeningProvider {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        Ok(ChainInfo {
            best_hash: self.block_hash,
            best_number: self.block_number,
        })
    }

    fn best_block_number(&self) -> ProviderResult<u64> {
        Ok(self.block_number)
    }

    fn last_block_number(&self) -> ProviderResult<u64> {
        Ok(self.block_number)
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        Ok((hash == self.block_hash).then_some(self.block_number))
    }
}

impl BlockIdReader for LysisOpeningProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash::new(self.block_number, self.block_hash)))
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash::new(self.block_number, self.block_hash)))
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash::new(self.block_number, self.block_hash)))
    }
}

impl StateProviderFactory for LysisOpeningProvider {
    fn latest(&self) -> ProviderResult<StateProviderBox> {
        Ok(Box::new(self.state.clone()))
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

    fn state_by_block_hash(&self, block: B256) -> ProviderResult<StateProviderBox> {
        assert_eq!(
            block, self.block_hash,
            "opening builder must request the exact finalized block hash"
        );
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
}

/// Builds a real q=3/4 finality certificate and real account/storage MPT paths.
///
/// Callers still cross the production verifier. The fixture merely replaces a
/// live four-node chain when a task-local process test needs deterministic
/// finalized bytes.
pub fn finalized_intent_proof_fixture(
    intent: JobIntentV1,
    limits: &SchemaLimits,
) -> FinalizedIntentProofFixture {
    build_finalized_intent_proof_fixture(intent, limits, Vec::new(), None).0
}

/// Builds one state root containing ValidatorSet, JobIntent, Fidelity and
/// Oracle, so the finality proof and both historical openings share the exact
/// authenticated block identity.
pub fn finalized_lysis_input_fixture(
    intent: JobIntentV1,
    bodies: &[TributeBodyV1],
    limits: &SchemaLimits,
) -> FinalizedLysisInputFixture {
    let mut reference_isos = bodies
        .iter()
        .map(|body| body.reference_currency)
        .collect::<std::collections::BTreeSet<_>>();
    reference_isos.insert(840);
    let subjects = OpeningSubjectsV1 {
        owners: bodies
            .iter()
            .map(|body| body.owner)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        reference_isos: reference_isos.into_iter().collect(),
    };
    let (fidelity_league_slots, oracle_contract) =
        lysis_contracts(WorldwideDay::new(intent.wwd), &subjects);
    let (finalized, opening_provider) = build_finalized_intent_proof_fixture(
        intent,
        limits,
        fidelity_league_slots,
        Some(oracle_contract),
    );
    FinalizedLysisInputFixture {
        finalized,
        opening_provider: opening_provider.expect("Lysis fixture includes opening contracts"),
        subjects,
    }
}

fn build_finalized_intent_proof_fixture(
    intent: JobIntentV1,
    limits: &SchemaLimits,
    fidelity_league_slots: Vec<(B256, U256)>,
    oracle_contract: Option<OpeningContractFixture>,
) -> (FinalizedIntentProofFixture, Option<LysisOpeningProvider>) {
    let canonical_job_intent = intent
        .encode_canonical(limits)
        .expect("fixture JobIntent is canonical");
    let intent_id = intent.intent_id(limits).expect("fixture IntentId");
    let logical_key = intent_storage_key(intent_id).expect("fixture intent storage key");
    let record = OcompJobRecordV1 {
        intent: intent.clone(),
        intent_height: intent.logical_evaluation_height,
        status: OcompJobStatus::AwaitingFinality,
        finalized: None,
        terminal: None,
    };
    let encoded_record = record
        .encode_canonical(limits)
        .expect("fixture job record is canonical");
    let intent_slots = dynamic_bytes_storage_slots(logical_key, &encoded_record);
    // The Fidelity league snapshot now lives in Metadosis storage, so its slots
    // share the intent account's storage trie under one storage root. The intent
    // finality proof and the league opening are two paths in the same trie.
    let mut metadosis_slots = intent_slots.clone();
    metadosis_slots.extend(
        fidelity_league_slots
            .iter()
            .map(|(slot, value)| (U256::from_be_bytes(slot.0), *value)),
    );
    let (metadosis_storage_root, metadosis_storage_proofs) = storage_trie(&metadosis_slots);
    let intent_storage_proofs = metadosis_storage_proofs[..intent_slots.len()].to_vec();
    let fidelity_storage_proofs = metadosis_storage_proofs[intent_slots.len()..].to_vec();
    let intent_account = TrieAccount {
        nonce: 0,
        balance: U256::ZERO,
        storage_root: metadosis_storage_root,
        code_hash: KECCAK_EMPTY,
    };

    let dkg = build_dkg();
    let snapshot = build_snapshot(&dkg);
    let committee_set_hash = snapshot.committee_set_hash_v2(FINALIZED_EPOCH);
    let committee_slots = committee_storage_slots(&snapshot, committee_set_hash);
    let snapshot_key = independent_snapshot_key(committee_set_hash);
    let ring_slot = U256::from(FINALIZED_EPOCH % 8).mapping_slot(U256::from(44));
    let mut validator_slots = committee_slots.clone();
    validator_slots.push((ring_slot, U256::from_be_bytes(snapshot_key.0)));
    let (validator_storage_root, validator_storage_proofs) = storage_trie(&validator_slots);
    let validator_account = TrieAccount {
        nonce: 0,
        balance: U256::ZERO,
        storage_root: validator_storage_root,
        code_hash: KECCAK_EMPTY,
    };
    // The Fidelity league opening shares the Metadosis intent account, so it is
    // NOT a distinct state account — only Oracle adds one. When no openings are
    // requested (proof-only fixtures), Metadosis and ValidatorSet are the only
    // accounts and no opening provider is produced.
    let (state_accounts, opening_contracts) = match oracle_contract {
        Some(oracle_contract) => {
            let fidelity_opening = OpeningContractFixture {
                address: METADOSIS_ADDRESS,
                slots: fidelity_league_slots,
                account: intent_account,
                storage_proofs: fidelity_storage_proofs,
            };
            (
                vec![
                    (METADOSIS_ADDRESS, intent_account),
                    (VALIDATOR_SET_ADDRESS, validator_account),
                    (ORACLE_ADDRESS, oracle_contract.account),
                ],
                vec![fidelity_opening, oracle_contract],
            )
        }
        None => (
            vec![
                (METADOSIS_ADDRESS, intent_account),
                (VALIDATOR_SET_ADDRESS, validator_account),
            ],
            Vec::new(),
        ),
    };
    let (state_root, account_proofs) = account_trie(&state_accounts);
    let header = OutbeHeader::new(Header {
        number: intent.logical_evaluation_height,
        state_root,
        ..Header::default()
    });
    let mut canonical_header = Vec::new();
    header.encode(&mut canonical_header);
    let header_hash = keccak256(&canonical_header);
    let block = ConsensusBlock::from_sealed(SealedBlock::seal_slow(OutbeBlock {
        header,
        body: Default::default(),
    }));
    assert_eq!(block.block_hash(), header_hash);

    let finalization = finalization_bytes(&dkg, header_hash);
    let signer_bitmap = signer_bitmap();
    let ordered_committee = snapshot
        .committee
        .iter()
        .map(|entry| entry.address)
        .collect::<Vec<_>>();
    let vrf_group_public_key_hash = keccak256(&snapshot.vrf_group_public_key_bytes);
    let parent_accounting = CertifiedParentAccountingMetadataV2 {
        finalized_block_number: intent.logical_evaluation_height,
        finalized_block_hash: header_hash,
        finalized_epoch: FINALIZED_EPOCH,
        finalized_view: FINALIZED_VIEW,
        parent_view: PARENT_VIEW,
        ordered_committee: ordered_committee
            .iter()
            .map(|address| BoundedBytes(address.as_slice().to_vec()))
            .collect(),
        signer_bitmap: BoundedBytes(signer_bitmap),
        canonical_commonware_finalization_proof: ProofBytes(finalization.clone()),
        committee_set_hash,
        vrf_material_version: VRF_MATERIAL_VERSION as u16,
        vrf_group_public_key_hash,
        proof_kind: ParentProofKind::Finalization,
        missed_proposers: Vec::new(),
    };
    let proof = FinalizedIntentProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        canonical_request_header_rlp: ProofBytes(canonical_header),
        parent_accounting,
        historical_committee_membership_proof: ProofBytes(historical_committee_witness(
            &snapshot,
            validator_account,
            &account_proofs[&VALIDATOR_SET_ADDRESS],
            &validator_storage_proofs[..committee_slots.len()],
        )),
        canonical_job_intent: BoundedBytes(canonical_job_intent),
        intent_account_proof: ProofBytes(account_witness(
            intent_account,
            &account_proofs[&METADOSIS_ADDRESS],
        )),
        intent_storage_proof: ProofBytes(storage_witness(&intent_storage_proofs)),
    };
    let job_id = intent
        .job_id(header_hash, state_root, limits)
        .expect("fixture JobId");
    let canonical_history =
        CanonicalHistoryFixture::new(intent.logical_evaluation_height, header_hash);
    let mut public_account_proofs = BTreeMap::new();
    public_account_proofs.insert(
        METADOSIS_ADDRESS,
        PublicAccountProofV1 {
            address: METADOSIS_ADDRESS,
            nonce: intent_account.nonce,
            balance: intent_account.balance,
            storage_root: intent_account.storage_root,
            code_hash: intent_account.code_hash,
            account_nodes: account_proofs[&METADOSIS_ADDRESS].clone(),
            storage_proofs: intent_slots
                .iter()
                .zip(&intent_storage_proofs)
                .map(|((slot, value), nodes)| PublicStorageProofV1 {
                    key: B256::new(slot.to_be_bytes::<32>()),
                    value: *value,
                    nodes: nodes.clone(),
                })
                .collect(),
        },
    );
    public_account_proofs.insert(
        VALIDATOR_SET_ADDRESS,
        PublicAccountProofV1 {
            address: VALIDATOR_SET_ADDRESS,
            nonce: validator_account.nonce,
            balance: validator_account.balance,
            storage_root: validator_account.storage_root,
            code_hash: validator_account.code_hash,
            account_nodes: account_proofs[&VALIDATOR_SET_ADDRESS].clone(),
            storage_proofs: validator_slots
                .iter()
                .zip(&validator_storage_proofs)
                .map(|((slot, value), nodes)| PublicStorageProofV1 {
                    key: B256::new(slot.to_be_bytes::<32>()),
                    value: *value,
                    nodes: nodes.clone(),
                })
                .collect(),
        },
    );
    let public_exact_block = PublicExactBlockFixtureV1 {
        finalization_bytes: finalization,
        block_bytes: block.encode().to_vec(),
        block_view: PublicBlockViewV1 {
            hash: header_hash,
            state_root,
            number: intent.logical_evaluation_height,
        },
        intent_id,
        canonical_job_record: encoded_record,
        account_proofs: public_account_proofs,
    };
    let finalized = FinalizedIntentProofFixture {
        intent,
        intent_id,
        job_id,
        proof,
        state_root,
        header_hash,
        block,
        canonical_history,
        public_exact_block,
    };
    let opening_provider = (!opening_contracts.is_empty()).then(|| {
        let mut accounts = BTreeMap::new();
        let mut storage = BTreeMap::new();
        let mut proofs = BTreeMap::new();
        for contract in opening_contracts {
            let account = Account {
                nonce: contract.account.nonce,
                balance: contract.account.balance,
                bytecode_hash: None,
            };
            accounts.insert(contract.address, account);
            storage.extend(
                contract
                    .slots
                    .iter()
                    .map(|(slot, value)| ((contract.address, *slot), *value)),
            );
            let storage_proofs = contract
                .slots
                .iter()
                .zip(contract.storage_proofs)
                .map(|((slot, value), nodes)| {
                    StorageProof {
                        key: *slot,
                        value: *value,
                        ..StorageProof::new(*slot)
                    }
                    .with_proof(nodes)
                })
                .collect();
            proofs.insert(
                contract.address,
                AccountProof {
                    address: contract.address,
                    info: Some(account),
                    proof: account_proofs[&contract.address].clone(),
                    storage_root: contract.account.storage_root,
                    storage_proofs,
                },
            );
        }
        LysisOpeningProvider {
            state: LysisOpeningState {
                state_root,
                accounts,
                storage,
                proofs,
            },
            block_number: finalized.block.number(),
            block_hash: finalized.header_hash,
        }
    });
    (finalized, opening_provider)
}

struct OpeningContractFixture {
    address: Address,
    slots: Vec<(B256, U256)>,
    account: TrieAccount,
    storage_proofs: Vec<Vec<Bytes>>,
}

/// A deterministic, valid Fidelity league for populating a fixture snapshot slot.
///
/// This is NOT the Fidelity league derivation — that lives in `outbe_fidelity`
/// (`league_from_rcfi`, RCFI → league). These fixtures mock the on-chain state a
/// node would read, so each snapshot slot needs *some* value in the canonical
/// `[MIN_LEAGUE, MAX_LEAGUE]` range. The value is opaque to the tests, which
/// assert opening-proof layout and deterministic re-execution — never league
/// semantics. A distinct (but arbitrary) per-owner value just spreads tributes
/// across more than one Lysis per-league group; owner order carries no meaning.
pub fn fixture_league(owner_index: usize) -> u16 {
    let span = usize::from(MAX_LEAGUE - MIN_LEAGUE) + 1;
    MIN_LEAGUE + u16::try_from(owner_index % span).unwrap_or(0)
}

/// Returns the per-owner Fidelity league snapshot slots (which live in Metadosis
/// storage and are merged into the intent account's storage trie by the caller)
/// plus the standalone Oracle opening contract.
fn lysis_contracts(
    day: WorldwideDay,
    subjects: &OpeningSubjectsV1,
) -> (Vec<(B256, U256)>, OpeningContractFixture) {
    // Owner order (subjects.owners is strictly ordered) matches the node's
    // `ordered_league_snapshot_slots`, so the opening's slot order is canonical.
    let fidelity_league_slots: Vec<(B256, U256)> = subjects
        .owners
        .iter()
        .enumerate()
        .map(|(owner_index, owner)| {
            (
                league_snapshot_slot(day.value(), *owner),
                U256::from(fixture_league(owner_index)),
            )
        })
        .collect();

    let reference_currency_count = u32::try_from(subjects.reference_isos.len())
        .expect("fixture reference ISO set fits the frozen profile");
    let oracle_counts = oracle_count_slot_plan_v1(day, &subjects.reference_isos)
        .expect("fixture reference ISO set is canonical");
    let oracle_plan = oracle_opening_slot_plan_v1(
        day,
        &subjects.reference_isos,
        reference_currency_count,
        1,
        1,
        0,
    )
    .expect("fixture Oracle counts fit the frozen profile");
    // Default every slot to a non-zero filler, then pin the words the
    // evaluator actually interprets. Pair-id slots stay non-zero so each ISO
    // resolves to a registered pair.
    let mut oracle_values = oracle_plan
        .slots
        .iter()
        .enumerate()
        .map(|(index, slot)| (*slot, U256::from(index + 1)))
        .collect::<BTreeMap<_, _>>();
    // The on-chain reference-currency list the subject ISOs must be a subset of.
    // Plan order is: length, then one element slot per registered currency.
    for (index, iso) in subjects.reference_isos.iter().enumerate() {
        oracle_values.insert(oracle_plan.slots[index + 1], U256::from(*iso));
    }
    oracle_values.insert(oracle_counts.slots[0], U256::from(reference_currency_count));
    oracle_values.insert(oracle_counts.slots[1], U256::from(1));
    oracle_values.insert(oracle_counts.slots[2], U256::from(1));
    oracle_values.insert(oracle_counts.slots[3], U256::from(1));
    oracle_values.insert(oracle_counts.slots[4], U256::ZERO);

    (
        fidelity_league_slots,
        opening_contract(ORACLE_ADDRESS, oracle_values.into_iter().collect()),
    )
}

fn opening_contract(address: Address, slots: Vec<(B256, U256)>) -> OpeningContractFixture {
    let words = slots
        .iter()
        .map(|(slot, value)| (U256::from_be_bytes(slot.0), *value))
        .collect::<Vec<_>>();
    let (storage_root, storage_proofs) = storage_trie(&words);
    OpeningContractFixture {
        address,
        slots,
        account: TrieAccount {
            nonce: 1,
            balance: U256::from(10),
            storage_root,
            code_hash: KECCAK_EMPTY,
        },
        storage_proofs,
    }
}

struct Dkg {
    keys: Vec<PrivateKey>,
    vrf_group_public_key: <MinSig as Variant>::Public,
    vrf_threshold_private: commonware_cryptography::bls12381::primitives::group::Private,
}

fn build_dkg() -> Dkg {
    let keys = (1..=4).map(PrivateKey::from_seed).collect();
    let mut rng = StdRng::seed_from_u64(13);
    let (vrf_threshold_private, vrf_group_public_key) = keypair::<_, MinSig>(&mut rng);
    Dkg {
        keys,
        vrf_group_public_key,
        vrf_threshold_private,
    }
}

fn build_snapshot(dkg: &Dkg) -> CommitteeSnapshot {
    let committee = dkg
        .keys
        .iter()
        .enumerate()
        .map(|(index, key)| {
            let encoded = key.public_key().encode();
            let mut consensus_pubkey = [0_u8; 48];
            consensus_pubkey.copy_from_slice(encoded.as_ref());
            CommitteeEntry {
                address: Address::with_last_byte((index + 1) as u8),
                consensus_pubkey,
            }
        })
        .collect();
    CommitteeSnapshot {
        committee,
        vrf_material_version: VRF_MATERIAL_VERSION,
        vrf_group_public_key_bytes: dkg.vrf_group_public_key.encode().to_vec(),
        vrf_public_polynomial_hash: B256::ZERO,
    }
}

fn proposal(header_hash: B256) -> Proposal<Sha256Digest> {
    Proposal::new(
        Round::new(Epoch::new(FINALIZED_EPOCH), View::new(FINALIZED_VIEW)),
        View::new(PARENT_VIEW),
        Sha256Digest(header_hash.0),
    )
}

fn finalization_bytes(dkg: &Dkg, header_hash: B256) -> Vec<u8> {
    let proposal = proposal(header_hash);
    let committee = Set::<PublicKey>::from_iter_dedup(dkg.keys.iter().map(PrivateKey::public_key));
    let namespace = finalize_namespace(&committee);
    let vote_message = proposal.encode().to_vec();
    let signatures = SIGNER_INDICES
        .iter()
        .map(|index| dkg.keys[*index as usize].sign(&namespace, &vote_message))
        .collect::<Vec<_>>();
    let bls_aggregated_vote =
        aggregate::combine_signatures::<MinPk, _>(signatures.iter().map(AsRef::as_ref));
    let seed_message = proposal.round.encode().to_vec();
    let threshold_signature = sign_message::<MinSig>(
        &dkg.vrf_threshold_private,
        &hybrid_seed_namespace(),
        &seed_message,
    );
    Finalization::<HybridScheme<MinSig>, Sha256Digest> {
        proposal,
        certificate: HybridCertificate::<MinSig> {
            signers: Signers::from(
                dkg.keys.len(),
                SIGNER_INDICES.iter().copied().map(Participant::new),
            ),
            bls_aggregated_vote,
            vrf_proof: Some(VrfProof {
                material_version: VRF_MATERIAL_VERSION,
                threshold_signature,
            }),
        },
    }
    .encode()
    .to_vec()
}

fn signer_bitmap() -> Vec<u8> {
    let mut bitmap = vec![0_u8; 4];
    for index in SIGNER_INDICES {
        bitmap[index as usize] = 1;
    }
    bitmap
}

fn dynamic_bytes_storage_slots(logical_key: B256, encoded: &[u8]) -> Vec<(U256, U256)> {
    let base = logical_key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT));
    if encoded.len() <= 31 {
        let mut inline = [0_u8; 32];
        inline[..encoded.len()].copy_from_slice(encoded);
        inline[31] = (encoded.len() * 2) as u8;
        return vec![(base, U256::from_be_bytes(inline))];
    }
    let mut slots = Vec::with_capacity(1 + encoded.len().div_ceil(32));
    slots.push((base, U256::from(encoded.len() * 2 + 1)));
    let data_base = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
    for (index, chunk) in encoded.chunks(32).enumerate() {
        let mut word = [0_u8; 32];
        word[..chunk.len()].copy_from_slice(chunk);
        slots.push((data_base + U256::from(index), U256::from_be_bytes(word)));
    }
    slots
}

fn storage_trie(slots: &[(U256, U256)]) -> (B256, Vec<Vec<Bytes>>) {
    let targets = slots
        .iter()
        .map(|(slot, _)| Nibbles::unpack(keccak256(slot.to_be_bytes::<32>())))
        .collect::<Vec<_>>();
    let mut leaves = BTreeMap::new();
    for ((_, word), target) in slots.iter().zip(&targets) {
        if !word.is_zero() {
            leaves.insert(*target, alloy_rlp::encode_fixed_size(word).to_vec());
        }
    }
    let mut builder =
        HashBuilder::default().with_proof_retainer(ProofRetainer::from_iter(targets.clone()));
    for (path, value) in leaves {
        builder.add_leaf(path, &value);
    }
    let root = builder.root();
    let retained = builder.take_proof_nodes();
    let proofs = targets
        .iter()
        .map(|target| {
            retained
                .matching_nodes_sorted(target)
                .into_iter()
                .map(|(_, node)| node)
                .collect()
        })
        .collect();
    (root, proofs)
}

fn account_trie(accounts: &[(Address, TrieAccount)]) -> (B256, BTreeMap<Address, Vec<Bytes>>) {
    let targets = accounts
        .iter()
        .map(|(address, _)| (*address, Nibbles::unpack(keccak256(address))))
        .collect::<BTreeMap<_, _>>();
    let mut builder = HashBuilder::default()
        .with_proof_retainer(ProofRetainer::from_iter(targets.values().copied()));
    let mut leaves = accounts
        .iter()
        .map(|(address, account)| (targets[address], alloy_rlp::encode(*account)))
        .collect::<Vec<_>>();
    leaves.sort_by_key(|(path, _)| *path);
    for (path, value) in leaves {
        builder.add_leaf(path, &value);
    }
    let root = builder.root();
    let retained = builder.take_proof_nodes();
    let proofs = targets
        .into_iter()
        .map(|(address, target)| {
            let proof = retained
                .matching_nodes_sorted(&target)
                .into_iter()
                .map(|(_, node)| node)
                .collect();
            (address, proof)
        })
        .collect();
    (root, proofs)
}

fn committee_storage_slots(
    snapshot: &CommitteeSnapshot,
    committee_set_hash: B256,
) -> Vec<(U256, U256)> {
    let key = independent_snapshot_key(committee_set_hash);
    let mapped = |slot: u64| key.mapping_slot(U256::from(slot));
    let nested = |slot: u64, index: u64| index.mapping_slot(mapped(slot));
    let mut slots = vec![
        (mapped(31), U256::from(1)),
        (mapped(32), U256::from(snapshot.committee.len())),
    ];
    for (index, entry) in snapshot.committee.iter().enumerate() {
        let index = index as u64;
        let mut high = [0_u8; 32];
        high[..16].copy_from_slice(&entry.consensus_pubkey[32..]);
        slots.extend([
            (
                nested(33, index),
                U256::from_be_slice(entry.address.as_slice()),
            ),
            (
                nested(34, index),
                U256::from_be_bytes::<32>(entry.consensus_pubkey[..32].try_into().unwrap()),
            ),
            (nested(35, index), U256::from_be_bytes(high)),
        ]);
    }
    slots.extend([
        (mapped(36), U256::from(snapshot.vrf_material_version)),
        (
            mapped(37),
            U256::from_be_bytes(keccak256(&snapshot.vrf_group_public_key_bytes).0),
        ),
        (
            mapped(38),
            U256::from(snapshot.vrf_group_public_key_bytes.len()),
        ),
    ]);
    for (index, chunk) in snapshot.vrf_group_public_key_bytes.chunks(32).enumerate() {
        let mut word = [0_u8; 32];
        word[..chunk.len()].copy_from_slice(chunk);
        slots.push((nested(39, index as u64), U256::from_be_bytes(word)));
    }
    slots.push((
        mapped(47),
        U256::from_be_bytes(snapshot.vrf_public_polynomial_hash.0),
    ));
    slots
}

fn independent_snapshot_key(committee_set_hash: B256) -> B256 {
    let mut preimage = Vec::new();
    preimage.extend_from_slice(b"OUTBE_COMMITTEE_SNAPSHOT_KEY_V2");
    preimage.extend_from_slice(&FINALIZED_EPOCH.to_be_bytes());
    preimage.extend_from_slice(committee_set_hash.as_slice());
    keccak256(preimage)
}

fn push_nodes(encoded: &mut Vec<u8>, nodes: &[Bytes]) {
    encoded.extend_from_slice(
        &u32::try_from(nodes.len())
            .expect("fixture node count fits u32")
            .to_be_bytes(),
    );
    for node in nodes {
        encoded.extend_from_slice(
            &u32::try_from(node.len())
                .expect("fixture node length fits u32")
                .to_be_bytes(),
        );
        encoded.extend_from_slice(node);
    }
}

fn account_witness(account: TrieAccount, nodes: &[Bytes]) -> Vec<u8> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(b"OAPI");
    encoded.extend_from_slice(&1_u16.to_be_bytes());
    encoded.extend_from_slice(&account.nonce.to_be_bytes());
    encoded.extend_from_slice(&account.balance.to_be_bytes::<32>());
    encoded.extend_from_slice(account.storage_root.as_slice());
    encoded.extend_from_slice(account.code_hash.as_slice());
    push_nodes(&mut encoded, nodes);
    encoded
}

fn storage_witness(proofs: &[Vec<Bytes>]) -> Vec<u8> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(b"OSPI");
    encoded.extend_from_slice(&1_u16.to_be_bytes());
    encoded.extend_from_slice(
        &u32::try_from(proofs.len())
            .expect("fixture proof count fits u32")
            .to_be_bytes(),
    );
    for proof in proofs {
        push_nodes(&mut encoded, proof);
    }
    encoded
}

fn historical_committee_witness(
    snapshot: &CommitteeSnapshot,
    validator_account: TrieAccount,
    account_nodes: &[Bytes],
    storage_proofs: &[Vec<Bytes>],
) -> Vec<u8> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(b"OCHI");
    encoded.extend_from_slice(&1_u16.to_be_bytes());
    encoded.extend_from_slice(
        &u32::try_from(snapshot.committee.len())
            .expect("fixture committee length fits u32")
            .to_be_bytes(),
    );
    for entry in &snapshot.committee {
        encoded.extend_from_slice(entry.address.as_slice());
        encoded.extend_from_slice(&entry.consensus_pubkey);
    }
    encoded.extend_from_slice(&snapshot.vrf_material_version.to_be_bytes());
    encoded.extend_from_slice(
        &u32::try_from(snapshot.vrf_group_public_key_bytes.len())
            .expect("fixture VRF key length fits u32")
            .to_be_bytes(),
    );
    encoded.extend_from_slice(&snapshot.vrf_group_public_key_bytes);
    encoded.extend_from_slice(snapshot.vrf_public_polynomial_hash.as_slice());
    let account = account_witness(validator_account, account_nodes);
    encoded.extend_from_slice(
        &u32::try_from(account.len())
            .expect("fixture account witness length fits u32")
            .to_be_bytes(),
    );
    encoded.extend_from_slice(&account);
    let storage = storage_witness(storage_proofs);
    encoded.extend_from_slice(
        &u32::try_from(storage.len())
            .expect("fixture storage witness length fits u32")
            .to_be_bytes(),
    );
    encoded.extend_from_slice(&storage);
    encoded
}
