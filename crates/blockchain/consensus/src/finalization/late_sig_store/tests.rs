use super::*;
use crate::proof::committee::{committee_set_hash_v2, CommitteeEntry, CommitteeSnapshot};
use crate::proof::constants::finalize_namespace;
use crate::proof::verify_late_finalize_proof;
use commonware_consensus::simplex::types::Proposal;
use commonware_consensus::types::{Epoch, Round, View};
use commonware_cryptography::{bls12381, Signer as _};
use commonware_math::algebra::Random;

fn keys(n: usize) -> Vec<bls12381::PrivateKey> {
    (0..n)
        .map(|_| {
            bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
        })
        .collect()
}

fn snapshot_for(keys: &[bls12381::PrivateKey]) -> CommitteeSnapshot {
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
    CommitteeSnapshot {
        committee,
        vrf_material_version: 1,
        vrf_group_public_key_bytes: vec![0x11; 96],
        vrf_public_polynomial_hash: alloy_primitives::B256::ZERO,
    }
}

struct FinalizeProposalFixture {
    epoch: u64,
    view: u64,
    parent_view: u64,
    fb_hash: B256,
}

/// Sign the canonical finalize message for `(epoch, view, parent_view, fb_hash)`
/// with `key` and return the raw MinPk signature the store stores. The
/// finalize namespace binds the full `committee`, so the signature only
/// verifies under that committee.
fn finalize_sig(
    committee: &[bls12381::PrivateKey],
    key: &bls12381::PrivateKey,
    proposal: FinalizeProposalFixture,
) -> MinPkSig {
    let FinalizeProposalFixture {
        epoch,
        view,
        parent_view,
        fb_hash,
    } = proposal;
    let committee_set: commonware_utils::ordered::Set<bls12381::PublicKey> =
        commonware_utils::ordered::Set::from_iter_dedup(
            committee
                .iter()
                .map(|k| bls12381::PublicKey::from(k.clone())),
        );
    let proposal = Proposal::new(
        Round::new(Epoch::new(epoch), View::new(view)),
        View::new(parent_view),
        crate::digest::Digest(fb_hash),
    );
    let message = proposal.encode().to_vec();
    let signature = key.sign(&finalize_namespace(&committee_set), &message);
    let inner: &MinPkSig = signature.as_ref();
    *inner
}

/// End-to-end: record votes -> resolve to a number -> build artifact ->
/// the resulting credit verifies through the Phase-4 verifier.
#[test]
fn record_resolve_build_round_trips_through_verifier() {
    let keys = keys(4);
    let snapshot = snapshot_for(&keys);
    let csh = committee_set_hash_v2(3, &snapshot);
    let fb = B256::repeat_byte(0xAB);
    let (epoch, view, parent_view, fb_number) = (3u64, 9u64, 8u64, 10u64);

    let mut store = LateFinalizeSigStore::new(3);
    for i in [0u32, 1, 2] {
        store.record_bound_vote(
            FinalizeVoteTarget {
                epoch,
                view,
                parent_view,
                fb_hash: fb,
            },
            i,
            finalize_sig(
                &keys,
                &keys[i as usize],
                FinalizeProposalFixture {
                    epoch,
                    view,
                    parent_view,
                    fb_hash: fb,
                },
            ),
        );
    }
    store.resolve_finalized_target(
        FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash: fb,
        },
        fb_number,
        LateFinalizeCommittee {
            set_hash: csh,
            size: snapshot.committee.len(),
        },
    );

    // Proposer of block 11: target 10 is in [11-3, 10] = [8, 10].
    let artifact = store.build_artifact(11);
    assert_eq!(artifact.batches.len(), 1);
    let credit = &artifact.batches[0];
    assert_eq!(credit.fb_number, fb_number);
    assert_eq!(
        verify_late_finalize_proof(&snapshot, credit).expect("late credit verifies"),
        vec![0, 1, 2]
    );
}

#[test]
fn build_artifact_excludes_out_of_window_targets() {
    let keys = keys(4);
    let snapshot = snapshot_for(&keys);
    let csh = committee_set_hash_v2(1, &snapshot);
    let fb = B256::repeat_byte(0x01);
    let mut store = LateFinalizeSigStore::new(3);
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch: 1,
            view: 9,
            parent_view: 8,
            fb_hash: fb,
        },
        0,
        finalize_sig(
            &keys,
            &keys[0],
            FinalizeProposalFixture {
                epoch: 1,
                view: 9,
                parent_view: 8,
                fb_hash: fb,
            },
        ),
    );
    store.resolve_finalized_target(
        FinalizeVoteTarget {
            epoch: 1,
            view: 9,
            parent_view: 8,
            fb_hash: fb,
        },
        10,
        LateFinalizeCommittee {
            set_hash: csh,
            size: 4,
        },
    );

    // Proposer at block 14: window [11, 13] - target 10 is too old.
    assert!(store.build_artifact(14).batches.is_empty());
    // Proposer at block 13: window [10, 12] - target 10 is the lower edge.
    assert_eq!(store.build_artifact(13).batches.len(), 1);
}

#[test]
fn resolve_prunes_targets_below_window() {
    let keys = keys(4);
    let snapshot = snapshot_for(&keys);
    // Distinct fb hashes for the two finalized blocks (fb_hash is unique).
    let fb = B256::repeat_byte(0x02);
    let fb2 = B256::repeat_byte(0x22);
    let mut store = LateFinalizeSigStore::new(3);
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch: 1,
            view: 9,
            parent_view: 8,
            fb_hash: fb,
        },
        0,
        finalize_sig(
            &keys,
            &keys[0],
            FinalizeProposalFixture {
                epoch: 1,
                view: 9,
                parent_view: 8,
                fb_hash: fb,
            },
        ),
    );
    store.resolve_finalized_target(
        FinalizeVoteTarget {
            epoch: 1,
            view: 9,
            parent_view: 8,
            fb_hash: fb,
        },
        10,
        LateFinalizeCommittee {
            set_hash: committee_set_hash_v2(1, &snapshot),
            size: 4,
        },
    );
    assert_eq!(store.resolved_len(), 1);
    // A much later finalization prunes the old target (10 < 100 - 3).
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch: 1,
            view: 99,
            parent_view: 98,
            fb_hash: fb2,
        },
        0,
        finalize_sig(
            &keys,
            &keys[0],
            FinalizeProposalFixture {
                epoch: 1,
                view: 99,
                parent_view: 98,
                fb_hash: fb2,
            },
        ),
    );
    store.resolve_finalized_target(
        FinalizeVoteTarget {
            epoch: 1,
            view: 99,
            parent_view: 98,
            fb_hash: fb2,
        },
        100,
        LateFinalizeCommittee {
            set_hash: committee_set_hash_v2(1, &snapshot),
            size: 4,
        },
    );
    assert_eq!(store.resolved_len(), 1);
}

#[test]
fn duplicate_signer_vote_is_ignored() {
    let keys = keys(4);
    let fb = B256::repeat_byte(0x03);
    let mut store = LateFinalizeSigStore::new(3);
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch: 1,
            view: 9,
            parent_view: 8,
            fb_hash: fb,
        },
        0,
        finalize_sig(
            &keys,
            &keys[0],
            FinalizeProposalFixture {
                epoch: 1,
                view: 9,
                parent_view: 8,
                fb_hash: fb,
            },
        ),
    );
    // Second vote from the same signer/fb_hash must not overwrite or duplicate.
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch: 1,
            view: 9,
            parent_view: 8,
            fb_hash: fb,
        },
        0,
        finalize_sig(
            &keys,
            &keys[0],
            FinalizeProposalFixture {
                epoch: 1,
                view: 9,
                parent_view: 8,
                fb_hash: fb,
            },
        ),
    );
    store.resolve_finalized_target(
        FinalizeVoteTarget {
            epoch: 1,
            view: 9,
            parent_view: 8,
            fb_hash: fb,
        },
        10,
        LateFinalizeCommittee {
            set_hash: B256::ZERO,
            size: 4,
        },
    );
    let artifact = store.build_artifact(11);
    assert_eq!(artifact.batches.len(), 1);
    // One signer bit set.
    assert_eq!(
        artifact.batches[0]
            .signer_bitmap
            .iter()
            .map(|b| b.count_ones())
            .sum::<u32>(),
        1
    );
}

/// A vote that arrives AFTER the block finalized (the slow-validator case) goes
/// into the already-resolved target and appears in the built artifact. It does
/// not stay stranded in the pending buffer.
#[test]
fn vote_arrives_after_resolution_is_included() {
    let keys = keys(4);
    let snapshot = snapshot_for(&keys);
    let csh = committee_set_hash_v2(3, &snapshot);
    let fb = B256::repeat_byte(0xCD);
    let (epoch, view, parent_view, fb_number) = (3u64, 9u64, 8u64, 10u64);

    let mut store = LateFinalizeSigStore::new(3);
    // Eager voter 0 before finalization.
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash: fb,
        },
        0,
        finalize_sig(
            &keys,
            &keys[0],
            FinalizeProposalFixture {
                epoch,
                view,
                parent_view,
                fb_hash: fb,
            },
        ),
    );
    store.resolve_finalized_target(
        FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash: fb,
        },
        fb_number,
        LateFinalizeCommittee {
            set_hash: csh,
            size: snapshot.committee.len(),
        },
    );

    // Slow voter 1 arrives AFTER finalization for the same fb_hash.
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash: fb,
        },
        1,
        finalize_sig(
            &keys,
            &keys[1],
            FinalizeProposalFixture {
                epoch,
                view,
                parent_view,
                fb_hash: fb,
            },
        ),
    );
    // Nothing left pending; both votes are in the resolved target.
    assert_eq!(store.pending_vote_count(fb), 0);

    let artifact = store.build_artifact(11);
    assert_eq!(artifact.batches.len(), 1);
    assert_eq!(
        verify_late_finalize_proof(&snapshot, &artifact.batches[0]).expect("verifies"),
        vec![0, 1],
        "both the eager and the late-arriving vote must be credited"
    );
}

/// Two proposals at the same view but with different fb_hash (equivocation /
/// fork) go into separate buffers. The store never aggregates them together.
/// Each resolves to its own target.
#[test]
fn equivocation_distinct_fb_hash_not_merged() {
    let keys = keys(4);
    let fb1 = B256::repeat_byte(0x11);
    let fb2 = B256::repeat_byte(0x22);
    let (epoch, view, parent_view) = (1u64, 9u64, 8u64);

    let mut store = LateFinalizeSigStore::new(3);
    // Same signer signs two different proposals at the same view.
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash: fb1,
        },
        0,
        finalize_sig(
            &keys,
            &keys[0],
            FinalizeProposalFixture {
                epoch,
                view,
                parent_view,
                fb_hash: fb1,
            },
        ),
    );
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash: fb2,
        },
        0,
        finalize_sig(
            &keys,
            &keys[0],
            FinalizeProposalFixture {
                epoch,
                view,
                parent_view,
                fb_hash: fb2,
            },
        ),
    );
    // Separate buffers - neither aggregate is poisoned by the other proposal.
    assert_eq!(store.pending_vote_count(fb1), 1);
    assert_eq!(store.pending_vote_count(fb2), 1);
}

/// A *pure* post-finalization vote must still build a credit that the verifier
/// accepts. "Pure" means that NOTHING was buffered before the block finalized.
/// `resolve_finalized_target` creates the target with the canonical
/// `(epoch, parent_view)` from the finalized certificate, not `0`/`0`. The
/// round-2 regression filled those with zero. So the rebuilt proposal
/// mismatched what the signer signed, and the aggregate failed.
#[test]
fn pure_post_finalization_vote_verifies() {
    let keys = keys(4);
    let snapshot = snapshot_for(&keys);
    let csh = committee_set_hash_v2(3, &snapshot);
    let fb = B256::repeat_byte(0xEF);
    // Non-zero epoch and parent_view - the zero-fill bug only shows up here.
    let (epoch, view, parent_view, fb_number) = (3u64, 9u64, 8u64, 10u64);

    let mut store = LateFinalizeSigStore::new(3);
    // Finalization happens with NO vote buffered for this fb_hash.
    store.resolve_finalized_target(
        FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash: fb,
        },
        fb_number,
        LateFinalizeCommittee {
            set_hash: csh,
            size: snapshot.committee.len(),
        },
    );
    assert_eq!(store.pending_vote_count(fb), 0);

    // The validator's own finalize vote arrives strictly after finalization.
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash: fb,
        },
        2,
        finalize_sig(
            &keys,
            &keys[2],
            FinalizeProposalFixture {
                epoch,
                view,
                parent_view,
                fb_hash: fb,
            },
        ),
    );

    let artifact = store.build_artifact(11);
    assert_eq!(artifact.batches.len(), 1);
    let credit = &artifact.batches[0];
    // The credit must carry the canonical binding, not 0/0.
    assert_eq!(credit.epoch, epoch);
    assert_eq!(credit.parent_view, parent_view);
    assert_eq!(
        verify_late_finalize_proof(&snapshot, credit)
            .expect("pure post-finalization credit must verify"),
        vec![2],
        "the post-finalization vote must be credited and verify"
    );
}

/// A finalize vote can be individually valid but signed over a DIFFERENT
/// `(epoch, view, parent_view)` for the same `fb_hash` (equivocation / a
/// Byzantine validator gossiping across views). The store must NOT merge such
/// a vote into the canonical aggregate. Otherwise it would make
/// `verify_same_message` fail and poison the whole per-block credit (a
/// chain-wide late-tail griefing vector). The store drops it on BOTH the
/// pending-merge path and the post-resolution late-arrival path. The artifact
/// still verifies over the honest, canonical-binding signers only.
#[test]
fn cross_view_same_fb_hash_vote_not_aggregated() {
    let keys = keys(4);
    let snapshot = snapshot_for(&keys);
    let csh = committee_set_hash_v2(3, &snapshot);
    let fb = B256::repeat_byte(0xC5);
    let (epoch, view, parent_view, fb_number) = (3u64, 9u64, 8u64, 10u64);
    let bad_view = 99u64; // a non-canonical view for the SAME fb_hash

    let mut store = LateFinalizeSigStore::new(3);
    // Pending-merge path: honest signer 0 at the canonical binding, plus a
    // cross-view signer 1 (valid over view=99). Both are buffered before resolve.
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash: fb,
        },
        0,
        finalize_sig(
            &keys,
            &keys[0],
            FinalizeProposalFixture {
                epoch,
                view,
                parent_view,
                fb_hash: fb,
            },
        ),
    );
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch,
            view: bad_view,
            parent_view,
            fb_hash: fb,
        },
        1,
        finalize_sig(
            &keys,
            &keys[1],
            FinalizeProposalFixture {
                epoch,
                view: bad_view,
                parent_view,
                fb_hash: fb,
            },
        ),
    );
    store.resolve_finalized_target(
        FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash: fb,
        },
        fb_number,
        LateFinalizeCommittee {
            set_hash: csh,
            size: snapshot.committee.len(),
        },
    );

    // Post-resolution late-arrival path: another cross-view signer 2.
    store.record_bound_vote(
        FinalizeVoteTarget {
            epoch,
            view: bad_view,
            parent_view,
            fb_hash: fb,
        },
        2,
        finalize_sig(
            &keys,
            &keys[2],
            FinalizeProposalFixture {
                epoch,
                view: bad_view,
                parent_view,
                fb_hash: fb,
            },
        ),
    );

    let artifact = store.build_artifact(11);
    assert_eq!(artifact.batches.len(), 1);
    let credit = &artifact.batches[0];
    // Only the canonical-binding signer 0 survived; the aggregate verifies.
    assert_eq!(
        verify_late_finalize_proof(&snapshot, credit)
            .expect("canonical-only aggregate must still verify"),
        vec![0],
        "cross-view votes for the same fb_hash must be dropped, not aggregated"
    );
}
