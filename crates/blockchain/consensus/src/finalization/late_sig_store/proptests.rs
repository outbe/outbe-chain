//! Property tests for [`super::LateFinalizeSigStore`].
//!
//! Signatures come from four `PrivateKey::from_seed` keys. A `OnceLock`
//! cache stores each `(signer, binding)` signature so cases stay fast and
//! reproducible. Proposer height 0 is outside `prop_window_membership`: a
//! genesis block has no predecessor, and production calls `build_artifact`
//! with `proposed_height >= 1`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

use alloy_primitives::B256;
use commonware_codec::Encode;
use commonware_consensus::simplex::types::Proposal;
use commonware_consensus::types::{Epoch, Round, View};
use commonware_cryptography::{bls12381, Signer};
use proptest::prelude::*;

use crate::proof::committee::{committee_set_hash_v2, CommitteeEntry, CommitteeSnapshot};
use crate::proof::constants::finalize_namespace;
use crate::proof::verify_late_finalize_proof;

use super::{
    FinalizeVoteTarget, LateFinalizeCommittee, LateFinalizeSigStore, MinPkSig, MAX_PENDING_TARGETS,
};

fn proptest_config() -> ProptestConfig {
    ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    }
}

type SigCacheKey = (u32, u64, u64, u64, B256);
type SigCache = BTreeMap<SigCacheKey, MinPkSig>;

struct LateSigFixture {
    keys: Vec<bls12381::PrivateKey>,
    snapshot: CommitteeSnapshot,
    sigs: Mutex<SigCache>,
}

fn fixture() -> &'static LateSigFixture {
    static FIXTURE: OnceLock<LateSigFixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let keys: Vec<_> = (0..4u64).map(bls12381::PrivateKey::from_seed).collect();
        let committee = keys
            .iter()
            .enumerate()
            .map(|(i, sk)| {
                let mut consensus_pubkey = [0u8; 48];
                consensus_pubkey.copy_from_slice(&sk.public_key().encode());
                CommitteeEntry {
                    address: alloy_primitives::Address::with_last_byte(i as u8 + 1),
                    consensus_pubkey,
                }
            })
            .collect();
        LateSigFixture {
            keys,
            snapshot: CommitteeSnapshot {
                committee,
                vrf_material_version: 1,
                vrf_group_public_key_bytes: vec![0x11; 96],
                vrf_public_polynomial_hash: B256::ZERO,
            },
            sigs: Mutex::new(BTreeMap::new()),
        }
    })
}

impl LateSigFixture {
    fn sign(
        &self,
        signer: u32,
        epoch: u64,
        view: u64,
        parent_view: u64,
        fb_hash: B256,
    ) -> MinPkSig {
        let cache_key = (signer, epoch, view, parent_view, fb_hash);
        if let Some(sig) = self
            .sigs
            .lock()
            .expect("sig cache")
            .get(&cache_key)
            .copied()
        {
            return sig;
        }
        let committee_set: commonware_utils::ordered::Set<bls12381::PublicKey> =
            commonware_utils::ordered::Set::from_iter_dedup(
                self.keys
                    .iter()
                    .map(|key| bls12381::PublicKey::from(key.clone())),
            );
        let proposal = Proposal::new(
            Round::new(Epoch::new(epoch), View::new(view)),
            View::new(parent_view),
            crate::digest::Digest(fb_hash),
        );
        let signature = self.keys[signer as usize]
            .sign(&finalize_namespace(&committee_set), &proposal.encode());
        let inner: &MinPkSig = signature.as_ref();
        let sig = *inner;
        self.sigs.lock().expect("sig cache").insert(cache_key, sig);
        sig
    }
}

fn hash_at(i: u32) -> B256 {
    let mut bytes = [0u8; 32];
    bytes[28..].copy_from_slice(&i.to_be_bytes());
    B256::from(bytes)
}

fn fixed_fb_hash() -> impl Strategy<Value = B256> {
    prop_oneof![
        Just(B256::repeat_byte(0x11)),
        Just(B256::repeat_byte(0x22)),
        Just(B256::repeat_byte(0x33)),
        Just(B256::repeat_byte(0x44)),
    ]
}

/// `choice == 0` is the canonical binding. The other three are distinct proposals
/// over the same `fb_hash`.
fn binding_for(choice: u8, canon: (u64, u64, u64)) -> (u64, u64, u64, bool) {
    let (epoch, view, parent_view) = canon;
    match choice % 4 {
        0 => (epoch, view, parent_view, true),
        1 => (epoch + 1, view, parent_view, false),
        2 => (epoch, view + 1, parent_view, false),
        _ => (epoch, view, parent_view + 1, false),
    }
}

fn record(
    store: &mut LateFinalizeSigStore,
    signer: u32,
    choice: u8,
    canon: (u64, u64, u64),
    fb_hash: B256,
) -> bool {
    let (epoch, view, parent_view, canonical) = binding_for(choice, canon);
    let sig = fixture().sign(signer, epoch, view, parent_view, fb_hash);
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash,
        },
        signer,
        sig,
    );
    canonical
}

/// Signers credited by the store's first-writer and canonical-binding rules.
///
/// A signer is credited when their first pre-resolution vote is canonical, or
/// when that pending vote did not credit them and a later post-resolution vote
/// is canonical. A non-canonical post-resolution vote does not occupy the slot.
fn oracle(pre: &[(u32, u8)], post: &[(u32, u8)]) -> Vec<usize> {
    let mut first_pre: BTreeMap<u32, bool> = BTreeMap::new();
    for &(signer, choice) in pre {
        let canonical = choice % 4 == 0;
        first_pre.entry(signer).or_insert(canonical);
    }
    let mut credited: BTreeSet<u32> = first_pre
        .iter()
        .filter(|(_, canonical)| **canonical)
        .map(|(&signer, _)| signer)
        .collect();
    for &(signer, choice) in post {
        if credited.contains(&signer) {
            continue;
        }
        if choice % 4 == 0 {
            credited.insert(signer);
        }
    }
    credited.into_iter().map(|signer| signer as usize).collect()
}

proptest! {
    #![proptest_config(proptest_config())]

    /// `fb_number` is in `build_artifact(proposer_block)` iff it sits in
    /// `[proposer_block - window_k, proposer_block)`.
    #[test]
    fn prop_window_membership(
        window_k in 1u64..=8,
        fb_number in 0u64..=500,
        proposer_block in 1u64..=520,
    ) {
        let fb_hash = hash_at(u32::try_from(fb_number).unwrap_or(u32::MAX));
        let canon = (1u64, 1u64, 0u64);
        let mut store = LateFinalizeSigStore::new(window_k);
        record(&mut store, 0, 0, canon, fb_hash);
        store.resolve_finalized_target(
            FinalizeVoteTarget {
                epoch: canon.0,
                view: canon.1,
                parent_view: canon.2,
                fb_hash,
            },
            fb_number,
            LateFinalizeCommittee {
                set_hash: B256::ZERO,
                size: 4,
            },
        );
        let included = store
            .build_artifact(proposer_block)
            .batches
            .iter()
            .any(|batch| batch.fb_number == fb_number);
        let expect =
            fb_number < proposer_block && fb_number >= proposer_block.saturating_sub(window_k);
        prop_assert_eq!(included, expect);
    }

    #[test]
    fn prop_cross_view_never_credited(
        canon in (1u64..=4u64, 1u64..=16u64).prop_flat_map(|(epoch, view)| {
            (0..view).prop_map(move |parent_view| (epoch, view, parent_view))
        }),
        fb_hash in fixed_fb_hash(),
        pre in proptest::collection::vec((0u32..4, 0u8..4), 0..=6),
        post in proptest::collection::vec((0u32..4, 0u8..4), 0..=6),
    ) {
        let expected = oracle(&pre, &post);
        let mut store = LateFinalizeSigStore::new(3);
        for &(signer, choice) in &pre {
            record(&mut store, signer, choice, canon, fb_hash);
        }
        let (epoch, view, parent_view) = canon;
        store.resolve_finalized_target(
            FinalizeVoteTarget {
                epoch,
                view,
                parent_view,
                fb_hash,
            },
            10,
            LateFinalizeCommittee {
                set_hash: committee_set_hash_v2(epoch, &fixture().snapshot),
                size: fixture().snapshot.committee.len(),
            },
        );
        for &(signer, choice) in &post {
            record(&mut store, signer, choice, canon, fb_hash);
        }
        let artifact = store.build_artifact(11);
        if expected.is_empty() {
            prop_assert!(artifact.batches.is_empty());
        } else {
            prop_assert_eq!(artifact.batches.len(), 1);
            let credited = verify_late_finalize_proof(&fixture().snapshot, &artifact.batches[0])
                .expect("canonical aggregate verifies");
            prop_assert_eq!(credited, expected);
        }
    }

    #[test]
    fn prop_first_writer_wins(signer in 0u32..4, fb_hash in fixed_fb_hash()) {
        for post in [false, true] {
            let canon = (3u64, 9u64, 8u64);
            let mut store = LateFinalizeSigStore::new(3);
            let committee = LateFinalizeCommittee {
                set_hash: committee_set_hash_v2(canon.0, &fixture().snapshot),
                size: fixture().snapshot.committee.len(),
            };
            let target = FinalizeVoteTarget {
                epoch: canon.0,
                view: canon.1,
                parent_view: canon.2,
                fb_hash,
            };
            if post {
                store.resolve_finalized_target(target, 10, committee);
            }
            record(&mut store, signer, 0, canon, fb_hash);
            record(&mut store, signer, 2, canon, fb_hash);
            if !post {
                store.resolve_finalized_target(target, 10, committee);
            }
            let artifact = store.build_artifact(11);
            prop_assert_eq!(artifact.batches.len(), 1);
            let bits: u32 = artifact.batches[0]
                .signer_bitmap
                .iter()
                .map(|byte| byte.count_ones())
                .sum();
            prop_assert_eq!(bits, 1);
            let credited =
                verify_late_finalize_proof(&fixture().snapshot, &artifact.batches[0])
                    .expect("first canonical signature verifies");
            prop_assert_eq!(credited, vec![signer as usize]);
        }
    }
}

#[test]
fn pending_cap_evicts_lowest_fb_hash() {
    let sig = fixture().sign(0, 1, 1, 0, hash_at(0));
    let mut store = LateFinalizeSigStore::new(3);
    let mut hashes = Vec::with_capacity(MAX_PENDING_TARGETS + 1);
    for i in 0..(MAX_PENDING_TARGETS as u32 + 1) {
        let fb_hash = hash_at(i);
        hashes.push(fb_hash);
        store.record_bound_vote(
            FinalizeVoteTarget {
                epoch: 1,
                view: 1,
                parent_view: 0,
                fb_hash,
            },
            0,
            sig,
        );
    }
    assert_eq!(store.pending_by_fb_hash.len(), MAX_PENDING_TARGETS);
    assert!(
        !store.pending_by_fb_hash.contains_key(&hashes[0]),
        "lowest fb_hash is evicted"
    );
    assert!(store
        .pending_by_fb_hash
        .contains_key(&hashes[hashes.len() - 1]));
}
