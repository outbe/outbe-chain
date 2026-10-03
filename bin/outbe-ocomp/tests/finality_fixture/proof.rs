use std::collections::BTreeMap;

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
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
use outbe_consensus::{
    hybrid::HybridScheme,
    proof::{
        constants::finalize_namespace, hybrid_seed_namespace, CommitteeEntry, CommitteeSnapshot,
        HybridCertificate, VrfProof,
    },
};
use outbe_fidelity::{MAX_LEAGUE, MIN_LEAGUE};
use outbe_metadosis::proof_layout::OCOMP_JOB_RECORDS_BASE_SLOT;
use outbe_nod::openings::entry_price_slots;
use outbe_ocomp_protocol::{league_snapshot::league_snapshot_slot, opening::OpeningSubjectsV1};
use outbe_primitives::{addresses::NOD_ADDRESS, storage::StorageKey, time::WorldwideDay};
use rand_commonware::{rngs::StdRng, SeedableRng as _};

use super::{FINALIZED_EPOCH, FINALIZED_VIEW, PARENT_VIEW, SIGNER_INDICES, VRF_MATERIAL_VERSION};

pub(crate) struct OpeningContractFixture {
    pub(crate) address: Address,
    pub(crate) slots: Vec<(B256, U256)>,
    pub(crate) account: TrieAccount,
    pub(crate) storage_proofs: Vec<Vec<Bytes>>,
}

/// A deterministic, valid Fidelity league for populating a fixture snapshot slot.
///
/// This is NOT the Fidelity league derivation - that lives in `outbe_fidelity`
/// (`league_from_rcfi`, RCFI -> league). These fixtures mock the on-chain state a
/// node would read, so each snapshot slot needs *some* value in the canonical
/// `[MIN_LEAGUE, MAX_LEAGUE]` range. The value is opaque to the tests, which
/// assert opening-proof layout and deterministic re-execution - never league
/// semantics. A distinct (but arbitrary) per-owner value just spreads tributes
/// across more than one Lysis per-league group; owner order carries no meaning.
pub fn fixture_league(owner_index: usize) -> u16 {
    let span = usize::from(MAX_LEAGUE - MIN_LEAGUE) + 1;
    MIN_LEAGUE + u16::try_from(owner_index % span).unwrap_or(0)
}

/// Returns the per-owner Fidelity league snapshot slots (which live in Metadosis
/// storage and are merged into the intent account's storage trie by the caller)
/// plus the standalone Oracle opening contract.
pub(crate) fn lysis_contracts(
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

    let oracle_values = entry_price_slots(day, &subjects.reference_isos)
        .expect("canonical Nod price subjects")
        .into_iter()
        .enumerate()
        .map(|(index, slot)| (slot, U256::from(index + 1)))
        .collect::<BTreeMap<_, _>>();

    (
        fidelity_league_slots,
        opening_contract(NOD_ADDRESS, oracle_values.into_iter().collect()),
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

pub(crate) struct Dkg {
    keys: Vec<PrivateKey>,
    vrf_group_public_key: <MinSig as Variant>::Public,
    vrf_threshold_private: commonware_cryptography::bls12381::primitives::group::Private,
}

pub(crate) fn build_dkg() -> Dkg {
    let keys = (1..=4).map(PrivateKey::from_seed).collect();
    let mut rng = StdRng::seed_from_u64(13);
    let (vrf_threshold_private, vrf_group_public_key) = keypair::<_, MinSig>(&mut rng);
    Dkg {
        keys,
        vrf_group_public_key,
        vrf_threshold_private,
    }
}

pub(crate) fn build_snapshot(dkg: &Dkg) -> CommitteeSnapshot {
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

pub(crate) fn finalization_bytes(dkg: &Dkg, header_hash: B256) -> Vec<u8> {
    let proposal = proposal(header_hash);
    let committee = Set::<PublicKey>::from_iter_dedup(dkg.keys.iter().map(PrivateKey::public_key));
    let namespace = finalize_namespace(&committee);
    let vote_message = proposal.encode().to_vec();
    let signatures = SIGNER_INDICES
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
    Finalization::<HybridScheme<MinSig>, Sha256Digest> {
        proposal,
        certificate: HybridCertificate::<MinSig> {
            signers: Signers::new(
                dkg.keys.len() as u32,
                SIGNER_INDICES.iter().copied().map(Participant::new),
            )
            .unwrap(),
            bls_aggregated_vote,
            vrf_proof: VrfProof {
                material_version: VRF_MATERIAL_VERSION,
                threshold_signature,
            },
        },
    }
    .encode()
    .to_vec()
}

pub(crate) fn signer_bitmap() -> Vec<u8> {
    let mut bitmap = vec![0_u8; 4];
    for index in SIGNER_INDICES {
        bitmap[index as usize] = 1;
    }
    bitmap
}

pub(crate) fn dynamic_bytes_storage_slots(logical_key: B256, encoded: &[u8]) -> Vec<(U256, U256)> {
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

pub(crate) fn storage_trie(slots: &[(U256, U256)]) -> (B256, Vec<Vec<Bytes>>) {
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

pub(crate) fn account_trie(
    accounts: &[(Address, TrieAccount)],
) -> (B256, BTreeMap<Address, Vec<Bytes>>) {
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

pub(crate) fn committee_storage_slots(
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

pub(crate) fn independent_snapshot_key(committee_set_hash: B256) -> B256 {
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

pub(crate) fn historical_committee_witness(
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
