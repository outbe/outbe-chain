use std::collections::BTreeMap;

use alloy_consensus::Header;
use alloy_primitives::{keccak256, B256, U256};
use alloy_rlp::Encodable as _;
use alloy_trie::{TrieAccount, KECCAK_EMPTY};
use commonware_codec::Encode as _;
use outbe_compressed_entities::TributeBodyV1;
use outbe_consensus::block::ConsensusBlock;
use outbe_node::ocomp::finality::{PublicAccountProofV1, PublicBlockViewV1, PublicStorageProofV1};
use outbe_ocomp_protocol::{
    common::{BoundedBytes, ProofBytes},
    intent::{
        intent_storage_key, CertifiedParentAccountingMetadataV2, FinalizedIntentProofV1,
        JobIntentV1, ParentProofKind,
    },
    opening::OpeningSubjectsV1,
    state::{OcompJobRecordV1, OcompJobStatus},
    SchemaLimits,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::{METADOSIS_ADDRESS, NOD_ADDRESS, VALIDATOR_SET_ADDRESS},
    header::OutbeHeader,
    storage::types::StorageKey as _,
    OutbeBlock,
};
use reth_primitives_traits::{Account, SealedBlock};
use reth_trie::{AccountProof, StorageProof};

use super::proof::{
    account_trie, account_witness, build_dkg, build_snapshot, committee_storage_slots,
    dynamic_bytes_storage_slots, finalization_bytes, historical_committee_witness,
    independent_snapshot_key, lysis_contracts, signer_bitmap, storage_trie, storage_witness,
    OpeningContractFixture,
};
use super::provider::{
    CanonicalHistoryFixture, FinalizedIntentProofFixture, FinalizedLysisInputFixture,
    LysisOpeningProvider, LysisOpeningState, PublicExactBlockFixtureV1,
};
use super::{FINALIZED_EPOCH, FINALIZED_VIEW, PARENT_VIEW, VRF_MATERIAL_VERSION};

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

/// Builds one state root that contains ValidatorSet, JobIntent, Fidelity and
/// Oracle. Thus the finality proof and both historical openings share the exact
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
    // NOT a distinct state account. Only Oracle adds one. When the caller requests
    // no openings (proof-only fixtures), Metadosis and ValidatorSet are the only
    // accounts and the fixture produces no opening provider.
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
                    (NOD_ADDRESS, oracle_contract.account),
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
                storage_reads: (),
                block_lookup: outbe_node::test_utils::NoBlockLookup,
            },
            block: outbe_node::test_utils::ExactBlockLookup {
                identity: alloy_eips::BlockNumHash::new(
                    finalized.block.number(),
                    finalized.header_hash,
                ),
            },
            exact_hash_message: "opening builder must request the exact finalized block hash",
        }
    });
    (finalized, opening_provider)
}
