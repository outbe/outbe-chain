//! Certified payout round, paid-leaf bitmap and batch verification.

use alloy_primitives::U256;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api;
use crate::payout::test_support::{
    contributor_leaf, contributor_range_proof, contributor_root, metadata_word,
};
use crate::payout::ContributorLeafData;

const CHAIN_ID: u64 = 1;
const WWD: u32 = 20_260_725;

fn with_registry<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(1_700_000_000_u64));
    StorageHandle::enter(&mut storage, f)
}

fn population(count: u32) -> Vec<ContributorLeafData> {
    (0..count)
        .map(|i| contributor_leaf(i, u64::from(i) + 1))
        .collect()
}

/// Installs the constant-size certified authority the activation path writes.
fn install_generation(storage: &StorageHandle<'_>, leaves: &[ContributorLeafData]) {
    let count = u32::try_from(leaves.len()).expect("count fits u32");
    let total = leaves
        .iter()
        .fold(U256::ZERO, |acc, leaf| acc + leaf.nominal);
    let registry = crate::IntexContract::new(storage.clone());
    registry
        .ocomp_contributor_root
        .write(&WWD, contributor_root(leaves))
        .unwrap();
    registry
        .ocomp_eligible_nominal_total
        .write(&WWD, total)
        .unwrap();
    registry
        .ocomp_contributor_metadata
        .write(&WWD, metadata_word(1, count))
        .unwrap();
}

#[test]
fn payout_round_opens_once() {
    with_registry(|storage| {
        assert!(api::certified_payout_round(&storage, WWD)
            .unwrap()
            .is_none());

        api::open_certified_payout_round(&storage, WWD, U256::from(1_000_u64)).unwrap();
        let round = api::certified_payout_round(&storage, WWD).unwrap().unwrap();
        assert_eq!(round.amount, U256::from(1_000_u64));
        assert_eq!(round.paid_so_far, U256::ZERO);
        assert_eq!(round.paid_leaf_count, 0);

        let err = api::open_certified_payout_round(&storage, WWD, U256::from(7_u64)).unwrap_err();
        assert!(format!("{err:?}").contains("already open"), "{err:?}");
    });
}

#[test]
fn paid_bitmap_blocks_a_second_payment() {
    with_registry(|storage| {
        api::open_certified_payout_round(&storage, WWD, U256::from(1_000_u64)).unwrap();

        api::require_certified_leaves_unpaid(&storage, WWD, 0, 100).unwrap();
        api::mark_certified_leaves_paid(&storage, WWD, 0, 100, U256::from(400_u64)).unwrap();

        let err = api::require_certified_leaves_unpaid(&storage, WWD, 0, 100).unwrap_err();
        assert!(format!("{err:?}").contains("already paid"), "{err:?}");

        let round = api::certified_payout_round(&storage, WWD).unwrap().unwrap();
        assert_eq!(round.paid_so_far, U256::from(400_u64));
        assert_eq!(round.paid_leaf_count, 100);
    });
}

#[test]
fn full_chunk_fills_exactly_one_word() {
    with_registry(|storage| {
        api::open_certified_payout_round(&storage, WWD, U256::from(1_000_u64)).unwrap();

        // 256 leaves must not overflow the `1 << len` mask construction.
        api::mark_certified_leaves_paid(&storage, WWD, 0, 256, U256::from(1_u64)).unwrap();
        assert_eq!(api::paid_leaves_word(&storage, WWD, 0).unwrap(), U256::MAX);

        // The next chunk lives in the next word and stays untouched.
        assert_eq!(api::paid_leaves_word(&storage, WWD, 1).unwrap(), U256::ZERO);
        api::require_certified_leaves_unpaid(&storage, WWD, 256, 256).unwrap();
    });
}

#[test]
fn batch_verifies_against_the_installed_root() {
    with_registry(|storage| {
        let leaves = population(600);
        install_generation(&storage, &leaves);

        for (start, len) in [(0_u32, 256_u32), (256, 256), (512, 88)] {
            let generation = api::verify_certified_contributor_batch(
                &storage,
                WWD,
                start,
                &leaves[start as usize..(start + len) as usize],
                &contributor_range_proof(&leaves, start),
            )
            .unwrap_or_else(|e| panic!("batch at {start} must verify: {e:?}"));
            assert_eq!(generation.contributor_count, 600);
        }
    });
}

#[test]
fn batch_with_a_tampered_leaf_is_rejected() {
    with_registry(|storage| {
        let leaves = population(600);
        install_generation(&storage, &leaves);

        let mut tampered = leaves[0..256].to_vec();
        tampered[7].nominal += U256::from(1_u64);
        let err = api::verify_certified_contributor_batch(
            &storage,
            WWD,
            0,
            &tampered,
            &contributor_range_proof(&leaves, 0),
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("does not match"), "{err:?}");
    });
}

#[test]
fn batch_without_a_certified_root_is_rejected() {
    with_registry(|storage| {
        let leaves = population(600);
        let err = api::verify_certified_contributor_batch(
            &storage,
            WWD,
            0,
            &leaves[0..256],
            &contributor_range_proof(&leaves, 0),
        )
        .unwrap_err();
        assert!(
            format!("{err:?}").contains("no certified contributor root"),
            "{err:?}"
        );
    });
}
