//! Shared deterministic fixture mechanics for independent OCOMP finality tests.

use std::collections::BTreeMap;

use alloy_consensus::Header;
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::Encodable as _;
use alloy_trie::{TrieAccount, KECCAK_EMPTY};
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
use outbe_consensus::{
    block::ConsensusBlock,
    hybrid::HybridScheme,
    proof::{
        constants::finalize_namespace, hybrid_seed_namespace, CommitteeEntry, CommitteeSnapshot,
        HybridCertificate, VrfProof,
    },
};
use outbe_metadosis::proof_layout::OCOMP_JOB_RECORDS_BASE_SLOT;
use outbe_ocomp_protocol::{
    common::{BoundedBytes, ProofBytes},
    intent::{
        CertifiedParentAccountingMetadataV2, FinalizedIntentProofV1, JobIntentV1, ParentProofKind,
    },
    state::{OcompJobRecordV1, OcompJobStatus},
    test_utils::{account_mpt_with_proofs, solidity_bytes_storage_slots, storage_trie},
};
use outbe_primitives::{
    addresses::{METADOSIS_ADDRESS, VALIDATOR_SET_ADDRESS},
    header::OutbeHeader,
    storage::StorageKey as _,
    OutbeBlock,
};
use rand_commonware::{rngs::StdRng, SeedableRng as _};
use reth_primitives_traits::SealedBlock;

/// Fixed test keys for a four-validator finality certificate.
pub(crate) struct FinalityFixtureKeys {
    keys: Vec<PrivateKey>,
    vrf_group_public_key: <MinSig as Variant>::Public,
    vrf_threshold_private: commonware_cryptography::bls12381::primitives::group::Private,
}

/// Build the same deterministic keys for each finality fixture.
pub(crate) fn deterministic_finality_keys() -> FinalityFixtureKeys {
    let keys = (1..=4).map(PrivateKey::from_seed).collect();
    let mut rng = StdRng::seed_from_u64(13);
    let (vrf_threshold_private, vrf_group_public_key) = keypair::<_, MinSig>(&mut rng);
    FinalityFixtureKeys {
        keys,
        vrf_group_public_key,
        vrf_threshold_private,
    }
}

impl FinalityFixtureKeys {
    pub(crate) fn snapshot(&self, vrf_material_version: u64) -> CommitteeSnapshot {
        committee_snapshot(
            &self.keys,
            self.vrf_group_public_key.encode().to_vec(),
            vrf_material_version,
        )
    }
}

/// Finalization coordinates for one test certificate.
#[derive(Clone, Copy)]
pub(crate) struct FinalizationCoordinates {
    pub(crate) epoch: u64,
    pub(crate) view: u64,
    pub(crate) parent_view: u64,
    pub(crate) vrf_material_version: u64,
}

/// Encode a real certificate for the selected test signers.
pub(crate) fn finalization_bytes(
    dkg: &FinalityFixtureKeys,
    signer_indices: &[u32],
    header_hash: B256,
    coordinates: FinalizationCoordinates,
) -> Vec<u8> {
    let proposal = Proposal::new(
        Round::new(Epoch::new(coordinates.epoch), View::new(coordinates.view)),
        View::new(coordinates.parent_view),
        Sha256Digest(header_hash.0),
    );
    let committee = Set::<PublicKey>::from_iter_dedup(dkg.keys.iter().map(PrivateKey::public_key));
    let namespace = finalize_namespace(&committee);
    let vote_message = proposal.encode().to_vec();
    let signatures = signer_indices
        .iter()
        .map(|index| dkg.keys[*index as usize].sign(&namespace, &vote_message))
        .collect::<Vec<_>>();
    let bls_aggregated_vote = aggregate::combine_signatures::<MinPk, _>(
        commonware_utils::iter::NonEmpty::try_new(signatures.iter().map(AsRef::as_ref)).unwrap(),
    );
    let seed_message = proposal.round.encode().to_vec();
    let threshold_signature = sign_message::<MinSig>(
        &dkg.vrf_threshold_private,
        &hybrid_seed_namespace(),
        &seed_message,
    );
    let certificate = HybridCertificate::<MinSig> {
        signers: Signers::new(
            dkg.keys.len() as u32,
            signer_indices.iter().copied().map(Participant::new),
        )
        .unwrap(),
        bls_aggregated_vote,
        vrf_proof: VrfProof {
            material_version: coordinates.vrf_material_version,
            threshold_signature,
        },
    };
    Finalization::<HybridScheme<MinSig>, Sha256Digest> {
        proposal,
        certificate,
    }
    .encode()
    .to_vec()
}

pub(crate) fn committee_snapshot(
    keys: &[PrivateKey],
    vrf_group_public_key_bytes: Vec<u8>,
    vrf_material_version: u64,
) -> CommitteeSnapshot {
    let committee = keys
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
        vrf_material_version,
        vrf_group_public_key_bytes,
        vrf_public_polynomial_hash: B256::ZERO,
    }
}

pub(crate) fn push_nodes(encoded: &mut Vec<u8>, nodes: &[Bytes]) {
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

pub(crate) fn account_witness(account: TrieAccount, nodes: &[Bytes]) -> Vec<u8> {
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

pub(crate) fn storage_witness(proofs: &[Vec<Bytes>]) -> Vec<u8> {
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

pub(crate) fn append_committee_slots(
    slots: &mut Vec<(U256, U256)>,
    snapshot: &CommitteeSnapshot,
    nested: &impl Fn(u64, u64) -> U256,
) {
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
}

pub(crate) fn historical_committee_witness(
    snapshot: &CommitteeSnapshot,
    account: &[u8],
    storage: &[u8],
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
    encoded.extend_from_slice(
        &u32::try_from(account.len())
            .expect("fixture account witness length fits u32")
            .to_be_bytes(),
    );
    encoded.extend_from_slice(account);
    encoded.extend_from_slice(
        &u32::try_from(storage.len())
            .expect("fixture storage witness length fits u32")
            .to_be_bytes(),
    );
    encoded.extend_from_slice(storage);
    encoded
}

pub(crate) fn independent_snapshot_key(epoch: u64, committee_set_hash: B256) -> B256 {
    let mut preimage = Vec::new();
    preimage.extend_from_slice(b"OUTBE_COMMITTEE_SNAPSHOT_KEY_V2");
    preimage.extend_from_slice(&epoch.to_be_bytes());
    preimage.extend_from_slice(committee_set_hash.as_slice());
    keccak256(preimage)
}

pub(crate) fn committee_storage_slots(
    snapshot: &CommitteeSnapshot,
    epoch: u64,
    committee_set_hash: B256,
) -> Vec<(U256, U256)> {
    let key = independent_snapshot_key(epoch, committee_set_hash);
    let mapped = |slot: u64| key.mapping_slot(U256::from(slot));
    let nested = |slot: u64, index: u64| index.mapping_slot(mapped(slot));
    let mut slots = vec![
        (mapped(31), U256::from(1)),
        (mapped(32), U256::from(snapshot.committee.len())),
    ];
    append_committee_slots(&mut slots, snapshot, &nested);
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

pub(crate) fn historical_committee_witness_from_proofs(
    snapshot: &CommitteeSnapshot,
    validator_account: TrieAccount,
    account_nodes: &[Bytes],
    storage_proofs: &[Vec<Bytes>],
) -> Vec<u8> {
    let account = account_witness(validator_account, account_nodes);
    let storage = storage_witness(storage_proofs);
    historical_committee_witness(snapshot, &account, &storage)
}

fn signer_bitmap(signer_indices: &[u32]) -> Vec<u8> {
    let mut bitmap = vec![0_u8; 4];
    for index in signer_indices {
        bitmap[*index as usize] = 1;
    }
    bitmap
}

/// Return the `AwaitingFinality` job record that Metadosis stores for `intent`.
pub(crate) fn awaiting_finality_record(intent: &JobIntentV1) -> OcompJobRecordV1 {
    OcompJobRecordV1 {
        intent: intent.clone(),
        intent_height: intent.logical_evaluation_height,
        status: OcompJobStatus::AwaitingFinality,
        finalized: None,
        terminal: None,
    }
}

/// The inputs of one finalized-intent proof assembly.
pub(crate) struct FinalizedIntentAssemblyInput<'a> {
    pub(crate) intent: &'a JobIntentV1,
    /// The canonical encoding of `intent`.
    pub(crate) canonical_job_intent: Vec<u8>,
    /// The storage key of the job record of `intent`.
    pub(crate) logical_key: B256,
    /// The canonical job record of `intent`.
    pub(crate) encoded_record: &'a [u8],
    /// More Metadosis slots in the storage trie of the intent account.
    pub(crate) extra_metadosis_slots: &'a [(U256, U256)],
    /// One more state account after the Metadosis and ValidatorSet accounts.
    pub(crate) extra_state_account: Option<(Address, TrieAccount)>,
    pub(crate) signer_indices: &'a [u32],
    pub(crate) coordinates: FinalizationCoordinates,
    pub(crate) finalized_block_number: u64,
}

/// A finalized-intent proof and the trie values that the callers use for
/// their providers.
pub(crate) struct FinalizedIntentAssembly {
    pub(crate) intent_slots: Vec<(U256, U256)>,
    /// The proofs of the intent slots, then the proofs of the extra Metadosis slots.
    pub(crate) metadosis_storage_proofs: Vec<Vec<Bytes>>,
    pub(crate) intent_account: TrieAccount,
    pub(crate) validator_slots: Vec<(U256, U256)>,
    /// The proofs of the committee slots, then the proof of the ring slot.
    pub(crate) validator_storage_proofs: Vec<Vec<Bytes>>,
    pub(crate) validator_account: TrieAccount,
    pub(crate) state_root: B256,
    pub(crate) account_proofs: BTreeMap<Address, Vec<Bytes>>,
    pub(crate) header: OutbeHeader,
    pub(crate) header_hash: B256,
    pub(crate) block: ConsensusBlock,
    pub(crate) finalization: Vec<u8>,
    pub(crate) signer_bitmap: Vec<u8>,
    pub(crate) ordered_committee: Vec<Address>,
    pub(crate) committee_set_hash: B256,
    pub(crate) vrf_group_public_key_hash: B256,
    pub(crate) proof: FinalizedIntentProofV1,
}

impl FinalizedIntentAssembly {
    /// Return the proofs of the intent slots without the extra Metadosis slots.
    pub(crate) fn intent_storage_proofs(&self) -> &[Vec<Bytes>] {
        &self.metadosis_storage_proofs[..self.intent_slots.len()]
    }
}

/// Build a real finalized-intent proof with real account and storage tries.
///
/// The Metadosis account holds the job record and the extra Metadosis slots.
/// The ValidatorSet account holds the committee snapshot and its ring entry.
pub(crate) fn assemble_finalized_intent_proof(
    input: FinalizedIntentAssemblyInput<'_>,
) -> FinalizedIntentAssembly {
    let FinalizedIntentAssemblyInput {
        intent,
        canonical_job_intent,
        logical_key,
        encoded_record,
        extra_metadosis_slots,
        extra_state_account,
        signer_indices,
        coordinates,
        finalized_block_number,
    } = input;
    let record_base = logical_key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT));
    let intent_slots = solidity_bytes_storage_slots(record_base, encoded_record);
    let mut metadosis_slots = intent_slots.clone();
    metadosis_slots.extend_from_slice(extra_metadosis_slots);
    let (metadosis_storage_root, metadosis_storage_proofs) = storage_trie(&metadosis_slots);
    let intent_account = TrieAccount {
        nonce: 0,
        balance: U256::ZERO,
        storage_root: metadosis_storage_root,
        code_hash: KECCAK_EMPTY,
    };

    let dkg = deterministic_finality_keys();
    let snapshot = dkg.snapshot(coordinates.vrf_material_version);
    let committee_set_hash = snapshot.committee_set_hash_v2(coordinates.epoch);
    let committee_slots = committee_storage_slots(&snapshot, coordinates.epoch, committee_set_hash);
    let mut validator_slots = committee_slots.clone();
    // The public builder authenticates the retained snapshot's ring entry first.
    // Keep it outside the canonical committee witness, which contains snapshot slots only.
    let ring_slot = U256::from(coordinates.epoch % 8).mapping_slot(U256::from(44));
    let snapshot_key = independent_snapshot_key(coordinates.epoch, committee_set_hash);
    validator_slots.push((ring_slot, U256::from_be_bytes(snapshot_key.0)));
    let (validator_storage_root, validator_storage_proofs) = storage_trie(&validator_slots);
    let validator_account = TrieAccount {
        nonce: 0,
        balance: U256::ZERO,
        storage_root: validator_storage_root,
        code_hash: KECCAK_EMPTY,
    };
    let mut state_accounts = vec![
        (METADOSIS_ADDRESS, intent_account),
        (VALIDATOR_SET_ADDRESS, validator_account),
    ];
    state_accounts.extend(extra_state_account);
    let (state_root, account_proofs) = account_mpt_with_proofs(&state_accounts);
    let header = OutbeHeader::new(Header {
        number: finalized_block_number,
        state_root,
        ..Header::default()
    });
    let mut canonical_header = Vec::new();
    header.encode(&mut canonical_header);
    let header_hash = keccak256(&canonical_header);
    let block = ConsensusBlock::from_sealed(SealedBlock::seal_slow(OutbeBlock {
        header: header.clone(),
        body: Default::default(),
    }));
    assert_eq!(block.block_hash(), header_hash);

    let finalization = finalization_bytes(&dkg, signer_indices, header_hash, coordinates);
    let signer_bitmap = signer_bitmap(signer_indices);
    let ordered_committee = snapshot
        .committee
        .iter()
        .map(|entry| entry.address)
        .collect::<Vec<_>>();
    let vrf_group_public_key_hash = keccak256(&snapshot.vrf_group_public_key_bytes);
    let parent_accounting = CertifiedParentAccountingMetadataV2 {
        finalized_block_number,
        finalized_block_hash: header_hash,
        finalized_epoch: coordinates.epoch,
        finalized_view: coordinates.view,
        parent_view: coordinates.parent_view,
        ordered_committee: ordered_committee
            .iter()
            .map(|address| BoundedBytes(address.as_slice().to_vec()))
            .collect(),
        signer_bitmap: BoundedBytes(signer_bitmap.clone()),
        canonical_commonware_finalization_proof: ProofBytes(finalization.clone()),
        committee_set_hash,
        vrf_material_version: coordinates.vrf_material_version as u16,
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
        historical_committee_membership_proof: ProofBytes(
            historical_committee_witness_from_proofs(
                &snapshot,
                validator_account,
                &account_proofs[&VALIDATOR_SET_ADDRESS],
                &validator_storage_proofs[..committee_slots.len()],
            ),
        ),
        canonical_job_intent: BoundedBytes(canonical_job_intent),
        intent_account_proof: ProofBytes(account_witness(
            intent_account,
            &account_proofs[&METADOSIS_ADDRESS],
        )),
        intent_storage_proof: ProofBytes(storage_witness(
            &metadosis_storage_proofs[..intent_slots.len()],
        )),
    };
    FinalizedIntentAssembly {
        intent_slots,
        metadosis_storage_proofs,
        intent_account,
        validator_slots,
        validator_storage_proofs,
        validator_account,
        state_root,
        account_proofs,
        header,
        header_hash,
        block,
        finalization,
        signer_bitmap,
        ordered_committee,
        committee_set_hash,
        vrf_group_public_key_hash,
        proof,
    }
}
