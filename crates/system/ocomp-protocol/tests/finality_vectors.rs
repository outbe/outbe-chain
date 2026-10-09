//! OCM-FIN-001: independent finalized-intent proof vectors.
//!
//! The fixture constructs real Commonware q=3/4 finalization bytes and real
//! Ethereum account/storage MPT proofs. Verification crosses the production
//! `FinalizedIntentProofV1::verify` seam. The fixture uses no accepting
//! verifier substitute.
// OCOMP-TEST-ID: OCM-FIN-001

#[path = "finality_vectors/public_builder.rs"]
mod public_builder;

#[path = "../../../../testing/ocomp_finality_fixture.rs"]
mod shared_finality_fixture;

use shared_finality_fixture::{
    assemble_finalized_intent_proof, awaiting_finality_record, FinalizationCoordinates,
    FinalizedIntentAssembly, FinalizedIntentAssemblyInput,
};

use outbe_ocomp_protocol::test_utils::{
    account_mpt_with_proofs as account_trie, activation_preconditions_fixture, job_intent_fixture,
    storage_trie, ActivationFixtureSource, ActivationFixtureTargets, JobIntentFixtureValues,
    FINALITY_INPUT_TEST_LIMITS as LIMITS,
};
use std::collections::BTreeMap;

use alloy_eips::{BlockHashOrNumber, BlockNumHash, BlockNumberOrTag};
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::Encodable as _;
use alloy_trie::{TrieAccount, KECCAK_EMPTY};
use outbe_consensus::{
    block::ConsensusBlock,
    finalization::parent_cert_store::{
        CertifiedParentProofRecord, CertifiedParentProofStore, FinalizedParentCertStore, ProofKind,
    },
};
use outbe_metadosis::proof_layout::OCOMP_JOB_RECORDS_BASE_SLOT;
use outbe_node::ocomp::finality::{
    authenticate_snapshot_handoff, FinalizedIntentVerifier, RethFinalizedIntentProofBuilder,
    TrieHistoricalCommitteeAuthority,
};
use outbe_node::ocomp::retention::{
    CandidatePinV1, FinalizedInputProofSource, RethFinalizedInputProofSource,
};
use outbe_ocomp_protocol::{
    common::{BoundedBytes, ProofBytes},
    control::{FinalizedIntentProofResponseV1, SnapshotHandoffV1},
    input::CheckpointIdentityV1,
    intent::{
        intent_storage_key, DayType, ExpectedFinalizedIntentBindingV1,
        FinalizedIntentAuthorityError, FinalizedIntentProofV1, FinalizedIntentVerificationError,
        FrozenMetadosisValuesV1, JobIntentV1,
    },
    state::{
        LysisTerminalV1, OcompFinalizedJobV1, OcompJobRecordV1, OcompJobStatus,
        OcompTerminalOutcome,
    },
    ProtocolError,
};
use outbe_primitives::{
    addresses::{METADOSIS_ADDRESS, VALIDATOR_SET_ADDRESS},
    header::OutbeHeader,
    storage::types::StorageKey as _,
    OutbeReceipt,
};
use reth_chainspec::ChainInfo;
use reth_primitives_traits::Account;
use reth_storage_api::{
    errors::provider::ProviderResult, BlockHashReader, BlockIdReader, BlockNumReader,
    HeaderProvider, ReceiptProvider, StateProofProvider, StateProviderBox, StateProviderFactory,
};
use reth_trie::{AccountProof, StorageProof, TrieInput};
use std::ops::{RangeBounds, RangeInclusive};

const FINALIZED_BLOCK_NUMBER: u64 = 1;
const FINALIZED_EPOCH: u64 = 2;
const FINALIZED_VIEW: u64 = 3;
const PARENT_VIEW: u64 = 2;
const VRF_MATERIAL_VERSION: u64 = 5;

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn intent() -> JobIntentV1 {
    let activation_preconditions = activation_preconditions_fixture(
        ActivationFixtureSource {
            wwd: 7,
            pending_nonce: 0,
            tribute_source_generation: 1,
            collection_key: hash(5),
            sealed_collection_root: hash(6),
            exact_count: 0,
            exact_nominal_total: U256::ZERO,
        },
        ActivationFixtureTargets {
            nod_target_generation: 1,
            namespace_root_before: hash(10),
            contributor_series_version: 1,
            metadosis_state_version: 1,
        },
    );
    job_intent_fixture(
        activation_preconditions,
        JobIntentFixtureValues {
            chain_id: 42,
            genesis_hash: hash(1),
            fork_id: hash(2),
            attempt: 0,
            protocol_bundle_hash: hash(3),
            ce_sealed_root: hash(4),
            pre_admission_envelope_hash: hash(7),
            source_availability_policy_id: hash(8),
            frozen_metadosis_values: FrozenMetadosisValuesV1 {
                day_type: DayType::Green,
                day_limit: U256::from(1_000),
                previous_vwap: U256::from(90),
                current_vwap: U256::from(100),
                gratis_demand: U256::from(25),
                day_gratis_limit_minor: U256::from(20),
                lysis_limit_minor: U256::from(300),
                desis_limit_minor: U256::from(700),
                request_limit_split_receipt_hash: hash(9),
            },
            logical_evaluation_height: FINALIZED_BLOCK_NUMBER,
            logical_evaluation_time: 1_000,
            result_validator_set_epoch: 1,
            result_committee_set_hash: hash(11),
            result_ocomp_binding_hash: hash(12),
            result_member_count: 4,
            result_quorum_threshold: 3,
            custody_committee_epoch_hash: None,
        },
    )
}

fn independent_storage_slots(logical_key: B256, encoded_record: &[u8]) -> Vec<(U256, U256)> {
    let base = logical_key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT));
    outbe_ocomp_protocol::test_utils::solidity_bytes_storage_slots(base, encoded_record)
}

#[test]
fn independent_bytes_oracle_matches_literal_boundary_words() {
    let first = U256::from_be_bytes(
        alloy_primitives::b256!("0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20")
            .0,
    );
    let second = U256::from_be_bytes(
        alloy_primitives::b256!("2122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40")
            .0,
    );
    let tail_33 = U256::from_be_bytes(
        alloy_primitives::b256!("2100000000000000000000000000000000000000000000000000000000000000")
            .0,
    );
    let tail_65 = U256::from_be_bytes(
        alloy_primitives::b256!("4100000000000000000000000000000000000000000000000000000000000000")
            .0,
    );
    let inline_1 = U256::from_be_bytes(
        alloy_primitives::b256!("0100000000000000000000000000000000000000000000000000000000000002")
            .0,
    );
    let inline_31 = U256::from_be_bytes(
        alloy_primitives::b256!("0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f3e")
            .0,
    );
    let logical_key = B256::repeat_byte(0x42);
    let base = logical_key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT));
    let data_base = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
    for (length, expected_words) in [
        (0_u8, vec![U256::ZERO]),
        (1, vec![inline_1]),
        (31, vec![inline_31]),
        (32, vec![U256::from(65), first]),
        (33, vec![U256::from(67), first, tail_33]),
        (64, vec![U256::from(129), first, second]),
        (65, vec![U256::from(131), first, second, tail_65]),
    ] {
        let input = (1..=length).collect::<Vec<_>>();
        let actual = independent_storage_slots(logical_key, &input);
        assert_eq!(actual.len(), expected_words.len(), "length {length}");
        for (index, ((slot, word), expected_word)) in
            actual.into_iter().zip(expected_words).enumerate()
        {
            let expected_slot = if index == 0 {
                base
            } else {
                data_base + U256::from(index - 1)
            };
            assert_eq!(slot, expected_slot, "length {length}, slot {index}");
            assert_eq!(word, expected_word, "length {length}, word {index}");
        }
    }
}

#[test]
fn ocm_fin_001_trie_root_and_proof_bytes_characterization() {
    fn append_proof(encoded: &mut Vec<u8>, proof: &[Bytes]) {
        encoded.extend_from_slice(&(proof.len() as u32).to_be_bytes());
        for node in proof {
            encoded.extend_from_slice(&(node.len() as u32).to_be_bytes());
            encoded.extend_from_slice(node);
        }
    }

    let slots = [
        (U256::from(1), U256::from(11)),
        (U256::from(2), U256::ZERO),
        (U256::from(3), U256::from(33)),
    ];
    let (storage_root, storage_proofs) = storage_trie(&slots);
    let (without_zero_root, _) = storage_trie(&[slots[0], slots[2]]);
    assert_eq!(storage_root, without_zero_root);

    let first = TrieAccount {
        nonce: 1,
        balance: U256::from(7),
        storage_root,
        code_hash: KECCAK_EMPTY,
    };
    let second = TrieAccount {
        nonce: 2,
        balance: U256::from(9),
        storage_root: KECCAK_EMPTY,
        code_hash: KECCAK_EMPTY,
    };
    let accounts = [
        (Address::repeat_byte(0x20), first),
        (Address::repeat_byte(0x10), second),
    ];
    let (account_root, account_proofs) = account_trie(&accounts);
    let (reverse_root, reverse_proofs) = account_trie(&[accounts[1], accounts[0]]);
    assert_eq!(account_root, reverse_root);
    assert_eq!(account_proofs, reverse_proofs);

    let mut encoded = Vec::new();
    encoded.extend_from_slice(storage_root.as_slice());
    encoded.extend_from_slice(&(storage_proofs.len() as u32).to_be_bytes());
    for proof in &storage_proofs {
        append_proof(&mut encoded, proof);
    }
    encoded.extend_from_slice(account_root.as_slice());
    encoded.extend_from_slice(&(account_proofs.len() as u32).to_be_bytes());
    for (address, proof) in &account_proofs {
        encoded.extend_from_slice(address.as_slice());
        append_proof(&mut encoded, proof);
    }
    assert_eq!(
        keccak256(encoded),
        alloy_primitives::b256!("b5aef948eafaddaf632d2090b06090fe858c921e7590414509112bbfea254511"),
    );
}

type FixtureStateProvider =
    outbe_node::test_utils::OpeningStateFixture<(), outbe_node::test_utils::ExactBlockLookup>;

#[derive(Clone)]
struct FixtureProvider {
    state: FixtureStateProvider,
    header: OutbeHeader,
}

impl BlockHashReader for FixtureProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        self.state.block_hash(number)
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        self.state.canonical_hashes_range(start, end)
    }
}

impl BlockNumReader for FixtureProvider {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        Ok(ChainInfo {
            best_hash: self.state.block_lookup.identity.hash,
            best_number: self.state.block_lookup.identity.number,
        })
    }

    fn best_block_number(&self) -> ProviderResult<u64> {
        Ok(self.state.block_lookup.identity.number)
    }

    fn last_block_number(&self) -> ProviderResult<u64> {
        Ok(self.state.block_lookup.identity.number)
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        Ok((hash == self.state.block_lookup.identity.hash)
            .then_some(self.state.block_lookup.identity.number))
    }
}

impl BlockIdReader for FixtureProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.chain_info()?.into()))
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.chain_info()?.into()))
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.chain_info()?.into()))
    }
}

impl HeaderProvider for FixtureProvider {
    type Header = OutbeHeader;

    fn header(&self, block_hash: B256) -> ProviderResult<Option<Self::Header>> {
        Ok((block_hash == self.state.block_lookup.identity.hash).then(|| self.header.clone()))
    }

    fn header_by_number(&self, number: u64) -> ProviderResult<Option<Self::Header>> {
        Ok((number == self.state.block_lookup.identity.number).then(|| self.header.clone()))
    }

    fn headers_range(&self, range: impl RangeBounds<u64>) -> ProviderResult<Vec<Self::Header>> {
        let contains = match (range.start_bound(), range.end_bound()) {
            (std::ops::Bound::Included(start), std::ops::Bound::Included(end)) => {
                *start <= self.state.block_lookup.identity.number
                    && self.state.block_lookup.identity.number <= *end
            }
            (std::ops::Bound::Included(start), std::ops::Bound::Excluded(end)) => {
                *start <= self.state.block_lookup.identity.number
                    && self.state.block_lookup.identity.number < *end
            }
            (std::ops::Bound::Excluded(start), std::ops::Bound::Included(end)) => {
                *start < self.state.block_lookup.identity.number
                    && self.state.block_lookup.identity.number <= *end
            }
            (std::ops::Bound::Excluded(start), std::ops::Bound::Excluded(end)) => {
                *start < self.state.block_lookup.identity.number
                    && self.state.block_lookup.identity.number < *end
            }
            (std::ops::Bound::Unbounded, std::ops::Bound::Included(end)) => {
                self.state.block_lookup.identity.number <= *end
            }
            (std::ops::Bound::Unbounded, std::ops::Bound::Excluded(end)) => {
                self.state.block_lookup.identity.number < *end
            }
            (std::ops::Bound::Included(start), std::ops::Bound::Unbounded) => {
                *start <= self.state.block_lookup.identity.number
            }
            (std::ops::Bound::Excluded(start), std::ops::Bound::Unbounded) => {
                *start < self.state.block_lookup.identity.number
            }
            (std::ops::Bound::Unbounded, std::ops::Bound::Unbounded) => true,
        };
        Ok(contains.then(|| self.header.clone()).into_iter().collect())
    }

    fn sealed_header(
        &self,
        number: u64,
    ) -> ProviderResult<Option<reth_primitives_traits::SealedHeader<Self::Header>>> {
        Ok(
            (number == self.state.block_lookup.identity.number).then(|| {
                reth_primitives_traits::SealedHeader::new(
                    self.header.clone(),
                    self.state.block_lookup.identity.hash,
                )
            }),
        )
    }

    fn sealed_headers_while(
        &self,
        range: impl RangeBounds<u64>,
        mut predicate: impl FnMut(&reth_primitives_traits::SealedHeader<Self::Header>) -> bool,
    ) -> ProviderResult<Vec<reth_primitives_traits::SealedHeader<Self::Header>>> {
        let headers = self.headers_range(range)?;
        let mut sealed = Vec::new();
        for header in headers {
            let header = reth_primitives_traits::SealedHeader::new(
                header,
                self.state.block_lookup.identity.hash,
            );
            if !predicate(&header) {
                break;
            }
            sealed.push(header);
        }
        Ok(sealed)
    }
}

impl ReceiptProvider for FixtureProvider {
    type Receipt = OutbeReceipt;

    fn receipt(&self, _id: u64) -> ProviderResult<Option<Self::Receipt>> {
        Ok(None)
    }

    fn receipt_by_hash(&self, _hash: B256) -> ProviderResult<Option<Self::Receipt>> {
        Ok(None)
    }

    fn receipts_by_block(
        &self,
        block: BlockHashOrNumber,
    ) -> ProviderResult<Option<Vec<Self::Receipt>>> {
        let known = match block {
            BlockHashOrNumber::Hash(hash) => hash == self.state.block_lookup.identity.hash,
            BlockHashOrNumber::Number(number) => number == self.state.block_lookup.identity.number,
        };
        Ok(known.then(Vec::new))
    }

    fn receipts_by_tx_range(
        &self,
        _range: impl RangeBounds<u64>,
    ) -> ProviderResult<Vec<Self::Receipt>> {
        Ok(Vec::new())
    }

    fn receipts_by_block_range(
        &self,
        _block_range: RangeInclusive<u64>,
    ) -> ProviderResult<Vec<Vec<Self::Receipt>>> {
        Ok(Vec::new())
    }
}

impl StateProviderFactory for FixtureProvider {
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
}

struct Fixture {
    intent: JobIntentV1,
    intent_id: B256,
    expected: ExpectedFinalizedIntentBindingV1,
    proof: FinalizedIntentProofV1,
    state_root: B256,
    header_hash: B256,
    block: ConsensusBlock,
    provider: FixtureProvider,
    finalization_record: CertifiedParentProofRecord,
}

impl Fixture {
    fn verify(
        &self,
        proof: &FinalizedIntentProofV1,
    ) -> Result<
        outbe_ocomp_protocol::intent::VerifiedFinalizedIntentV1,
        FinalizedIntentVerificationError,
    > {
        proof.verify(
            self.expected,
            &FinalizedIntentVerifier::new(TrieHistoricalCommitteeAuthority),
            &LIMITS,
        )
    }
}

fn fixture(signer_indices: &[u32]) -> Fixture {
    fixture_with_intent(signer_indices, intent())
}

fn fixture_with_intent(signer_indices: &[u32], intent: JobIntentV1) -> Fixture {
    let canonical_job_intent = intent.encode_canonical(&LIMITS).unwrap();
    let intent_id = intent.intent_id(&LIMITS).unwrap();
    let logical_key = intent_storage_key(intent_id).unwrap();
    let encoded_record = awaiting_finality_record(&intent)
        .encode_canonical(&LIMITS)
        .unwrap();
    let assembly = assemble_finalized_intent_proof(FinalizedIntentAssemblyInput {
        intent: &intent,
        canonical_job_intent,
        logical_key,
        encoded_record: &encoded_record,
        extra_metadosis_slots: &[],
        extra_state_account: None,
        signer_indices,
        coordinates: FinalizationCoordinates {
            epoch: FINALIZED_EPOCH,
            view: FINALIZED_VIEW,
            parent_view: PARENT_VIEW,
            vrf_material_version: VRF_MATERIAL_VERSION,
        },
        finalized_block_number: FINALIZED_BLOCK_NUMBER,
    });
    let intent_storage_proofs = assembly.intent_storage_proofs().to_vec();
    let FinalizedIntentAssembly {
        intent_slots: slots,
        intent_account,
        validator_slots,
        validator_storage_proofs: all_validator_storage_proofs,
        validator_account,
        state_root,
        account_proofs,
        header,
        header_hash,
        block,
        finalization,
        signer_bitmap: bitmap,
        ordered_committee,
        committee_set_hash,
        vrf_group_public_key_hash,
        proof,
        ..
    } = assembly;
    let finalization_record = CertifiedParentProofRecord {
        kind: ProofKind::Finalization {
            finalized_block_number: FINALIZED_BLOCK_NUMBER,
        },
        finalized_epoch: FINALIZED_EPOCH,
        finalized_view: FINALIZED_VIEW,
        parent_view: PARENT_VIEW,
        finalized_block_hash: header_hash,
        committee_set_hash,
        vrf_material_version: VRF_MATERIAL_VERSION,
        vrf_group_public_key_hash,
        ordered_committee,
        signer_bitmap: bitmap,
        encoded_proof: Bytes::from(finalization),
        stored_at_height: FINALIZED_BLOCK_NUMBER,
        ..CertifiedParentProofRecord::default()
    };
    let account = |trie: TrieAccount| Account {
        nonce: trie.nonce,
        balance: trie.balance,
        // Reth represents an account with empty code as `None`. The production
        // proof builder must derive KECCAK_EMPTY through `Account::get_bytecode_hash`.
        bytecode_hash: None,
    };
    let storage_proofs = |slots: &[(U256, U256)], proofs: &[Vec<Bytes>]| {
        slots
            .iter()
            .zip(proofs)
            .map(|((slot, value), nodes)| {
                let key = B256::new(slot.to_be_bytes::<32>());
                StorageProof {
                    key,
                    value: *value,
                    ..StorageProof::new(key)
                }
                .with_proof(nodes.clone())
            })
            .collect::<Vec<_>>()
    };
    let mut accounts = BTreeMap::new();
    accounts.insert(METADOSIS_ADDRESS, account(intent_account));
    accounts.insert(VALIDATOR_SET_ADDRESS, account(validator_account));
    let mut storage = BTreeMap::new();
    storage.extend(slots.iter().map(|(slot, value)| {
        (
            (METADOSIS_ADDRESS, B256::new(slot.to_be_bytes::<32>())),
            *value,
        )
    }));
    storage.extend(validator_slots.iter().map(|(slot, value)| {
        (
            (VALIDATOR_SET_ADDRESS, B256::new(slot.to_be_bytes::<32>())),
            *value,
        )
    }));
    let mut proofs = BTreeMap::new();
    proofs.insert(
        METADOSIS_ADDRESS,
        AccountProof {
            address: METADOSIS_ADDRESS,
            info: Some(account(intent_account)),
            proof: account_proofs[&METADOSIS_ADDRESS].clone(),
            storage_root: intent_account.storage_root,
            storage_proofs: storage_proofs(&slots, &intent_storage_proofs),
        },
    );
    proofs.insert(
        VALIDATOR_SET_ADDRESS,
        AccountProof {
            address: VALIDATOR_SET_ADDRESS,
            info: Some(account(validator_account)),
            proof: account_proofs[&VALIDATOR_SET_ADDRESS].clone(),
            storage_root: validator_account.storage_root,
            storage_proofs: storage_proofs(&validator_slots, &all_validator_storage_proofs),
        },
    );
    let provider = FixtureProvider {
        state: FixtureStateProvider {
            state_root,
            block_lookup: outbe_node::test_utils::ExactBlockLookup {
                identity: BlockNumHash::new(FINALIZED_BLOCK_NUMBER, header_hash),
            },
            accounts,
            storage,
            proofs,
            storage_reads: (),
        },
        header,
    };
    Fixture {
        expected: ExpectedFinalizedIntentBindingV1 {
            chain_id: intent.chain_id,
            genesis_hash: intent.genesis_hash,
            fork_id: intent.fork_id,
            protocol_bundle_hash: intent.protocol_bundle_hash,
        },
        intent,
        intent_id,
        proof,
        state_root,
        header_hash,
        block,
        provider,
        finalization_record,
    }
}

fn flip_first_account_trie_node(witness: &mut [u8]) {
    let node_count = u32::from_be_bytes(witness[110..114].try_into().unwrap());
    assert!(node_count > 0, "account vector must contain a trie node");
    let node_len = u32::from_be_bytes(witness[114..118].try_into().unwrap()) as usize;
    assert!(node_len > 0, "account trie node must be non-empty");
    witness[118] ^= 1;
}

fn flip_first_storage_trie_node(witness: &mut [u8]) {
    let proof_count = u32::from_be_bytes(witness[6..10].try_into().unwrap());
    assert!(proof_count > 0, "storage vector must contain a proof");
    let node_count = u32::from_be_bytes(witness[10..14].try_into().unwrap());
    assert!(
        node_count > 0,
        "first storage proof must contain a trie node"
    );
    let node_len = u32::from_be_bytes(witness[14..18].try_into().unwrap()) as usize;
    assert!(node_len > 0, "storage trie node must be non-empty");
    witness[18] ^= 1;
}

fn historical_account_witness_range(witness: &[u8]) -> std::ops::Range<usize> {
    assert_eq!(&witness[..4], b"OCHI");
    let committee_len = u32::from_be_bytes(witness[6..10].try_into().unwrap()) as usize;
    let mut offset = 10 + committee_len * (20 + 48) + 8;
    let vrf_key_len = u32::from_be_bytes(witness[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4 + vrf_key_len + 32;
    let account_len = u32::from_be_bytes(witness[offset..offset + 4].try_into().unwrap()) as usize;
    let start = offset + 4;
    start..start + account_len
}

fn historical_storage_witness_range(witness: &[u8]) -> std::ops::Range<usize> {
    let account = historical_account_witness_range(witness);
    let offset = account.end;
    let storage_len = u32::from_be_bytes(witness[offset..offset + 4].try_into().unwrap()) as usize;
    let start = offset + 4;
    start..start + storage_len
}

#[test]
fn ocm_fin_001_four_validators_derive_same_job_from_real_q3_finality_and_mpt() {
    let fixture = fixture(&[0, 1, 2]);
    assert_eq!(
        fixture.proof.parent_accounting.signer_bitmap.0,
        [1, 1, 1, 0],
        "the happy vector must prove the PoC's exact q=3/4 quorum"
    );

    let mut job_ids = Vec::new();
    for _validator_domain in 0..4 {
        let verified = fixture
            .verify(&fixture.proof)
            .expect("real finalized header, q=3 certificate, and MPT proofs must verify");
        assert_eq!(verified.intent, fixture.intent);
        assert_eq!(verified.request.block_hash, fixture.header_hash);
        assert_eq!(verified.request.state_root, fixture.state_root);
        assert_eq!(
            verified.job_id,
            fixture
                .intent
                .job_id(fixture.header_hash, fixture.state_root, &LIMITS)
                .unwrap()
        );
        job_ids.push(verified.job_id);
    }
    assert!(job_ids.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn ocm_fin_001_exporter_derives_job_inputs_only_from_verified_handoff_proof() {
    let baseline = fixture(&[0, 1, 2]);
    let verified = baseline
        .verify(&baseline.proof)
        .expect("baseline finality proof");
    let response = FinalizedIntentProofResponseV1 {
        job_id: verified.job_id,
        canonical_proof: BoundedBytes(
            baseline
                .proof
                .encode_canonical(&LIMITS)
                .expect("canonical finalized proof"),
        ),
    };
    let handoff = SnapshotHandoffV1 {
        job_id: verified.job_id,
        input_lease_id: verified.intent.input_lease_id().expect("input lease id"),
        pin_generation: 1,
        lease_generation: 2,
        checkpoint: CheckpointIdentityV1 {
            finalized_block_number: verified.request.block_number,
            finalized_block_hash: verified.request.block_hash,
            finalized_state_root: verified.request.state_root,
            finalized_ce_root: verified.intent.ce_sealed_root,
            ce_schema_version: 1,
        },
        canonical_lease_offer: BoundedBytes(vec![1]),
    };

    let authenticated =
        authenticate_snapshot_handoff(&handoff, &response, baseline.expected, &LIMITS)
            .expect("production exporter verifier closes every handoff binding");
    assert_eq!(authenticated, verified);

    let mut wrong_ce = handoff;
    wrong_ce.checkpoint.finalized_ce_root = hash(0xee);
    assert!(
        authenticate_snapshot_handoff(&wrong_ce, &response, baseline.expected, &LIMITS).is_err()
    );
}

#[test]
fn ocm_fin_001_production_builder_reconstructs_and_verifies_the_independent_vector() {
    let fixture = fixture(&[0, 1, 2]);
    let finalizations = FinalizedParentCertStore::new();
    finalizations
        .put_finalization(fixture.finalization_record.clone())
        .expect("persist exact Commonware finalization fixture");
    let builder =
        RethFinalizedIntentProofBuilder::new(fixture.provider.clone(), finalizations, LIMITS);

    let (built, verified) = builder
        .build_and_verify(&fixture.block, fixture.intent_id)
        .expect("production builder opens state, constructs MPT witnesses and self-verifies");

    assert_eq!(built, fixture.proof);
    assert_eq!(verified.intent_id, fixture.intent_id);
    assert_eq!(
        verified.job_id,
        fixture
            .intent
            .job_id(fixture.header_hash, fixture.state_root, &LIMITS)
            .expect("expected JobId")
    );
}

#[test]
fn ocm_fin_001_production_source_builds_exact_proof_and_refuses_missing_or_ambiguous_finality() {
    let fixture = fixture(&[0, 1, 2]);
    let candidate = CandidatePinV1 {
        block_number: FINALIZED_BLOCK_NUMBER,
        block_hash: fixture.header_hash,
        state_root: fixture.state_root,
        intent_id: fixture.intent_id,
        wwd: fixture.intent.wwd,
        ce_sealed_root: fixture.intent.ce_sealed_root,
        protocol_bundle_hash: fixture.intent.protocol_bundle_hash,
        input_lease_id: fixture.intent.input_lease_id().expect("input lease id"),
    };
    let exact_store = FinalizedParentCertStore::new();
    exact_store
        .put_finalization(fixture.finalization_record.clone())
        .expect("persist exact finalization");
    let exact_source = RethFinalizedInputProofSource::new(fixture.provider.clone(), exact_store);
    assert_eq!(
        exact_source
            .build_finalized_intent_proof(candidate)
            .expect("exact persisted finality resolves"),
        fixture.proof
    );

    let competing_store = FinalizedParentCertStore::new();
    let mut competing = fixture.finalization_record.clone();
    competing.finalized_block_hash = hash(0x99);
    competing_store
        .put_finalization(competing)
        .expect("persist competing finalization identity");
    let competing_source =
        RethFinalizedInputProofSource::new(fixture.provider.clone(), competing_store);
    assert!(competing_source
        .build_finalized_intent_proof(candidate)
        .is_err());

    let ambiguous_store = FinalizedParentCertStore::new();
    ambiguous_store
        .put_finalization(fixture.finalization_record.clone())
        .expect("persist first exact finalization");
    let mut duplicate = fixture.finalization_record.clone();
    duplicate.finalized_view += 1;
    ambiguous_store
        .put_finalization(duplicate)
        .expect("persist second exact-key variant");
    let ambiguous_source =
        RethFinalizedInputProofSource::new(fixture.provider.clone(), ambiguous_store);
    assert!(ambiguous_source
        .build_finalized_intent_proof(candidate)
        .is_err());
}

#[test]
fn ocm_fin_001_rejects_invented_historical_committee_values_and_paths() {
    let baseline = fixture(&[0, 1, 2]);

    let mut invented_member_key = baseline.proof.clone();
    invented_member_key.historical_committee_membership_proof.0[30] ^= 1;
    assert_eq!(
        baseline.verify(&invented_member_key),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::HistoricalCommittee,
        ))
    );

    let mut corrupt_validator_account = baseline.proof.clone();
    let account = historical_account_witness_range(
        &corrupt_validator_account
            .historical_committee_membership_proof
            .0,
    );
    flip_first_account_trie_node(
        &mut corrupt_validator_account
            .historical_committee_membership_proof
            .0[account],
    );
    assert_eq!(
        baseline.verify(&corrupt_validator_account),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::HistoricalCommittee,
        ))
    );

    let mut corrupt_validator_storage = baseline.proof.clone();
    let storage = historical_storage_witness_range(
        &corrupt_validator_storage
            .historical_committee_membership_proof
            .0,
    );
    flip_first_storage_trie_node(
        &mut corrupt_validator_storage
            .historical_committee_membership_proof
            .0[storage],
    );
    assert_eq!(
        baseline.verify(&corrupt_validator_storage),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::HistoricalCommittee,
        ))
    );
}

#[test]
fn ocm_fin_001_rejects_finality_committee_and_mpt_rebinding() {
    let baseline = fixture(&[0, 1, 2]);

    let mut wrong_request_state_root = baseline.proof.clone();
    let mut header = baseline.provider.header.clone();
    header.inner.state_root = hash(0x30);
    let mut canonical_header = Vec::new();
    header.encode(&mut canonical_header);
    wrong_request_state_root.canonical_request_header_rlp = ProofBytes(canonical_header);
    assert_eq!(
        baseline.verify(&wrong_request_state_root),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::CanonicalHeader,
        ))
    );

    let mut wrong_header_hash = baseline.proof.clone();
    wrong_header_hash.parent_accounting.finalized_block_hash = hash(0x31);
    assert_eq!(
        baseline.verify(&wrong_header_hash),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::CanonicalHeader,
        ))
    );

    let mut wrong_finalized_height = baseline.proof.clone();
    wrong_finalized_height
        .parent_accounting
        .finalized_block_number += 1;
    assert_eq!(
        baseline.verify(&wrong_finalized_height),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::CanonicalHeader,
        ))
    );

    let mut corrupt_finalization_certificate = baseline.proof.clone();
    let certificate = &mut corrupt_finalization_certificate
        .parent_accounting
        .canonical_commonware_finalization_proof
        .0;
    let last = certificate
        .last_mut()
        .expect("real finalization certificate is non-empty");
    *last ^= 1;
    assert_eq!(
        baseline.verify(&corrupt_finalization_certificate),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::ConsensusFinality,
        ))
    );

    let mut invented_committee = baseline.proof.clone();
    invented_committee.parent_accounting.ordered_committee[0] =
        BoundedBytes(Address::with_last_byte(0xfe).as_slice().to_vec());
    assert_eq!(
        baseline.verify(&invented_committee),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::ConsensusFinality,
        ))
    );

    let mut wrong_bitmap = baseline.proof.clone();
    wrong_bitmap.parent_accounting.signer_bitmap = BoundedBytes(vec![1, 1, 1, 1]);
    assert_eq!(
        baseline.verify(&wrong_bitmap),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::ConsensusFinality,
        ))
    );

    let below_quorum = fixture(&[0, 1]);
    assert_eq!(
        below_quorum.verify(&below_quorum.proof),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::ConsensusFinality,
        ))
    );

    let mut wrong_historical_opening = baseline.proof.clone();
    wrong_historical_opening
        .historical_committee_membership_proof
        .0[0] ^= 1;
    assert_eq!(
        baseline.verify(&wrong_historical_opening),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::HistoricalCommittee,
        ))
    );

    let mut corrupt_account_proof = baseline.proof.clone();
    flip_first_account_trie_node(&mut corrupt_account_proof.intent_account_proof.0);
    assert_eq!(
        baseline.verify(&corrupt_account_proof),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::IntentAccountProof,
        ))
    );

    let mut malformed_account_witness = baseline.proof.clone();
    malformed_account_witness.intent_account_proof.0[0] ^= 1;
    assert_eq!(
        baseline.verify(&malformed_account_witness),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::IntentAccountProof,
        ))
    );

    let mut corrupt_storage_proof = baseline.proof.clone();
    flip_first_storage_trie_node(&mut corrupt_storage_proof.intent_storage_proof.0);
    assert_eq!(
        baseline.verify(&corrupt_storage_proof),
        Err(FinalizedIntentVerificationError::Authority(
            FinalizedIntentAuthorityError::IntentStorageProof,
        ))
    );

    let mut wrong_chain = baseline.proof.clone();
    wrong_chain.chain_id += 1;
    assert_eq!(
        baseline.verify(&wrong_chain),
        Err(FinalizedIntentVerificationError::WrongChain)
    );

    let mut missed_proposer = baseline.proof.clone();
    missed_proposer
        .parent_accounting
        .missed_proposers
        .push(hash(0xaa));
    assert_eq!(
        baseline.verify(&missed_proposer),
        Err(FinalizedIntentVerificationError::NonEmptyMissedProposers)
    );
}

#[test]
fn finalized_intent_preflight_preserves_first_rejection() {
    let baseline = fixture(&[0, 1, 2]);
    let mut wrong_chain = baseline.proof.clone();
    wrong_chain.chain_id += 1;
    let mut wrong_genesis = baseline.proof.clone();
    wrong_genesis.genesis_hash = hash(0xa1);
    let mut wrong_fork = baseline.proof.clone();
    wrong_fork.fork_id = hash(0xa2);
    let mut wrong_bundle = baseline.proof.clone();
    wrong_bundle.protocol_bundle_hash = hash(0xa3);
    let mut missed = baseline.proof.clone();
    missed.parent_accounting.missed_proposers.push(hash(0xa4));
    for (proof, expected) in [
        (
            wrong_chain.clone(),
            FinalizedIntentVerificationError::WrongChain,
        ),
        (
            wrong_genesis,
            FinalizedIntentVerificationError::WrongGenesis,
        ),
        (wrong_fork, FinalizedIntentVerificationError::WrongFork),
        (
            wrong_bundle,
            FinalizedIntentVerificationError::WrongProtocolBundle,
        ),
        (
            missed,
            FinalizedIntentVerificationError::NonEmptyMissedProposers,
        ),
    ] {
        assert_eq!(baseline.verify(&proof), Err(expected));
    }

    // The chain rejection precedes header decoding and all authority calls.
    wrong_chain.canonical_request_header_rlp = ProofBytes(vec![0xff]);
    assert_eq!(
        baseline.verify(&wrong_chain),
        Err(FinalizedIntentVerificationError::WrongChain)
    );
}

#[test]
fn activation_precondition_groups_preserve_first_error() {
    let intent = intent();
    let mut preconditions = intent.activation_preconditions.clone();
    preconditions.tribute.wwd += 1;
    preconditions.metadosis.pending_nonce += 1;
    preconditions.tribute.exact_count += 1;
    assert_eq!(
        preconditions.validate_for_intent(&intent),
        Err(ProtocolError::InvalidInvariant(
            "activation precondition day binding"
        ))
    );
    preconditions.tribute.wwd = intent.wwd;
    assert_eq!(
        preconditions.validate_for_intent(&intent),
        Err(ProtocolError::InvalidInvariant(
            "activation precondition nonce binding"
        ))
    );
    preconditions.metadosis.pending_nonce = intent.pending_nonce;
    assert_eq!(
        preconditions.validate_for_intent(&intent),
        Err(ProtocolError::InvalidInvariant(
            "activation precondition source bounds"
        ))
    );
}

#[test]
fn job_status_matrix_preserves_errors_without_mutation() {
    let intent = intent();
    let block_hash = hash(0xb1);
    let state_root = hash(0xb2);
    let finalized = OcompFinalizedJobV1 {
        job_id: intent.job_id(block_hash, state_root, &LIMITS).unwrap(),
        finalized_request_block_hash: block_hash,
        finalized_request_state_root: state_root,
        finality_recorded_height: 102,
        open_height: 106,
        deadline_height: 110,
        quorum: None,
    };
    let terminal = LysisTerminalV1 {
        outcome: OcompTerminalOutcome::Expired,
        terminal_height: 110,
        terminal_time: 1_100,
        completed_binding: None,
    };
    let base = OcompJobRecordV1 {
        intent,
        intent_height: FINALIZED_BLOCK_NUMBER,
        status: OcompJobStatus::AwaitingFinality,
        finalized: None,
        terminal: None,
    };
    base.validate_semantics(&LIMITS).unwrap();
    let mut expired = base.clone();
    expired.status = OcompJobStatus::Expired;
    expired.finalized = Some(finalized.clone());
    expired.terminal = Some(terminal.clone());
    expired.validate_semantics(&LIMITS).unwrap();

    let mut wrong_shape = base.clone();
    wrong_shape.terminal = Some(terminal.clone());
    let mut wrong_expired = expired.clone();
    wrong_expired.terminal.as_mut().unwrap().outcome = OcompTerminalOutcome::Failed;
    let mut wrong_failed = expired.clone();
    wrong_failed.status = OcompJobStatus::Failed;
    let mut wrong_completed = expired.clone();
    wrong_completed.status = OcompJobStatus::Completed;
    let mut missing_binding = expired.clone();
    missing_binding.status = OcompJobStatus::Completed;
    missing_binding.terminal.as_mut().unwrap().outcome = OcompTerminalOutcome::Completed;
    for (record, expected) in [
        (wrong_shape, "job status terminal shape"),
        (wrong_expired, "expired terminal shape"),
        (wrong_failed, "failed terminal shape"),
        (wrong_completed, "completed terminal shape"),
        (missing_binding, "completed binding present"),
    ] {
        let before = record.clone();
        assert_eq!(
            record.validate_semantics(&LIMITS),
            Err(ProtocolError::InvalidInvariant(expected))
        );
        assert_eq!(record, before);
    }
}
