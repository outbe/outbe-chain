use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};
use commonware_codec::Encode as _;
use outbe_compressed_entities::TributeBodyV1;
use outbe_node::ocomp::finality::{PublicAccountProofV1, PublicBlockViewV1, PublicStorageProofV1};
use outbe_ocomp_protocol::{
    intent::{intent_storage_key, JobIntentV1},
    opening::OpeningSubjectsV1,
    SchemaLimits,
};
use outbe_primitives::addresses::{METADOSIS_ADDRESS, NOD_ADDRESS, VALIDATOR_SET_ADDRESS};
use outbe_primitives::time::WorldwideDay;
use reth_primitives_traits::Account;
use reth_trie::{AccountProof, StorageProof};

use super::proof::{
    assemble_finalized_intent_proof, awaiting_finality_record, lysis_contracts,
    FinalizationCoordinates, FinalizedIntentAssembly, FinalizedIntentAssemblyInput,
    OpeningContractFixture,
};
use super::provider::{
    CanonicalHistoryFixture, FinalizedIntentProofFixture, FinalizedLysisInputFixture,
    LysisOpeningProvider, LysisOpeningState, PublicExactBlockFixtureV1,
};
use super::{FINALIZED_EPOCH, FINALIZED_VIEW, PARENT_VIEW, SIGNER_INDICES, VRF_MATERIAL_VERSION};

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
    let encoded_record = awaiting_finality_record(&intent)
        .encode_canonical(limits)
        .expect("fixture job record is canonical");
    // The Fidelity league snapshot now lives in Metadosis storage, so its slots
    // share the intent account's storage trie under one storage root. The intent
    // finality proof and the league opening are two paths in the same trie.
    let fidelity_slot_words = fidelity_league_slots
        .iter()
        .map(|(slot, value)| (U256::from_be_bytes(slot.0), *value))
        .collect::<Vec<_>>();
    // The Fidelity league opening shares the Metadosis intent account, so it is
    // NOT a distinct state account. Only Oracle adds one. When the caller requests
    // no openings (proof-only fixtures), Metadosis and ValidatorSet are the only
    // accounts and the fixture produces no opening provider.
    let assembly = assemble_finalized_intent_proof(FinalizedIntentAssemblyInput {
        intent: &intent,
        canonical_job_intent,
        logical_key,
        encoded_record: &encoded_record,
        extra_metadosis_slots: &fidelity_slot_words,
        extra_state_account: oracle_contract
            .as_ref()
            .map(|oracle_contract| (NOD_ADDRESS, oracle_contract.account)),
        signer_indices: &SIGNER_INDICES,
        coordinates: FinalizationCoordinates {
            epoch: FINALIZED_EPOCH,
            view: FINALIZED_VIEW,
            parent_view: PARENT_VIEW,
            vrf_material_version: VRF_MATERIAL_VERSION,
        },
        finalized_block_number: intent.logical_evaluation_height,
    });
    let intent_storage_proofs = assembly.intent_storage_proofs().to_vec();
    let fidelity_storage_proofs =
        assembly.metadosis_storage_proofs[assembly.intent_slots.len()..].to_vec();
    let FinalizedIntentAssembly {
        intent_slots,
        intent_account,
        validator_slots,
        validator_storage_proofs,
        validator_account,
        state_root,
        account_proofs,
        header_hash,
        block,
        finalization,
        proof,
        ..
    } = assembly;
    let opening_contracts = match oracle_contract {
        Some(oracle_contract) => {
            let fidelity_opening = OpeningContractFixture {
                address: METADOSIS_ADDRESS,
                slots: fidelity_league_slots,
                account: intent_account,
                storage_proofs: fidelity_storage_proofs,
            };
            vec![fidelity_opening, oracle_contract]
        }
        None => Vec::new(),
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
