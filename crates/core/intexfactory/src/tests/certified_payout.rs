//! Creator-reward payout against a certified contributor root.

use alloy_primitives::{FixedBytes, B256, U256};
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;
use outbe_primitives::storage::StorageHandle;

use outbe_intex::payout::test_support::{
    contributor_leaf, contributor_range_proof, contributor_root, metadata_word,
};
use outbe_intex::payout::ContributorLeafData;

use super::{with_factory, DEADLINE_FUTURE};
use crate::constants::ORIGIN_ROUTER_ADDRESS;
use crate::precompile::IIntexFactory;
use crate::runtime;

const WWD: u32 = 20_260_725;
const CHAIN: u32 = 10;

fn population(count: u32) -> Vec<ContributorLeafData> {
    (0..count)
        .map(|i| contributor_leaf(i, u64::from(i) + 1))
        .collect()
}

fn nominal_total(leaves: &[ContributorLeafData]) -> U256 {
    leaves
        .iter()
        .fold(U256::ZERO, |acc, leaf| acc + leaf.nominal)
}

/// Writes the constant-size authority that OCOMP activation installs.
fn install_generation(storage: &StorageHandle<'_>, leaves: &[ContributorLeafData]) {
    install_generation_with_total(storage, leaves, nominal_total(leaves));
}

fn install_generation_with_total(
    storage: &StorageHandle<'_>,
    leaves: &[ContributorLeafData],
    eligible_nominal_total: U256,
) {
    let count = u32::try_from(leaves.len()).expect("count fits u32");
    let registry = outbe_intex::IntexContract::new(storage.clone());
    registry
        .ocomp_contributor_root
        .write(&WWD, contributor_root(leaves))
        .unwrap();
    registry
        .ocomp_eligible_nominal_total
        .write(&WWD, eligible_nominal_total)
        .unwrap();
    registry
        .ocomp_contributor_metadata
        .write(&WWD, metadata_word(1, count))
        .unwrap();
}

/// Installs an empty certified generation (a day whose contributors were all
/// excluded), which has a root but no payable leaves.
fn install_empty_generation(storage: &StorageHandle<'_>) {
    let registry = outbe_intex::IntexContract::new(storage.clone());
    registry
        .ocomp_contributor_root
        .write(&WWD, contributor_root(&[]))
        .unwrap();
    registry
        .ocomp_eligible_nominal_total
        .write(&WWD, U256::ZERO)
        .unwrap();
    registry
        .ocomp_contributor_metadata
        .write(&WWD, metadata_word(1, 0))
        .unwrap();
}

fn abi_leaves(leaves: &[ContributorLeafData]) -> Vec<IIntexFactory::ContributorLeaf> {
    leaves
        .iter()
        .map(|leaf| IIntexFactory::ContributorLeaf {
            owner: leaf.owner,
            sourceTributeDigest: B256::from_slice(&leaf.source_tribute_id[..32]),
            sourceTributeIndex: FixedBytes::<4>::from_slice(&leaf.source_tribute_id[32..]),
            nominal: leaf.nominal,
        })
        .collect()
}

/// Arms the fan-in and delivers the whole pot from one winning chain.
fn deliver_proceeds(storage: &StorageHandle<'_>, amount: U256) {
    outbe_intex::api::arm_proceeds(storage, WWD, &[CHAIN], DEADLINE_FUTURE).unwrap();
    storage
        .increase_balance(INTEX_FACTORY_ADDRESS, amount)
        .unwrap();
    runtime::distribute(storage, ORIGIN_ROUTER_ADDRESS, WWD, CHAIN, amount).unwrap();
}

#[test]
fn certified_day_opens_a_payout_round_instead_of_burning_the_pot() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        let amount = U256::from(1_000u64);

        deliver_proceeds(&s, amount);

        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .expect("certified day must open a payout round");
        assert_eq!(round.amount, amount);
        // The pot stays on the precompile until batches draw it down; the legacy
        // cursor path must not have claimed the day.
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), amount);
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
    });
}

#[test]
fn batches_pay_every_certified_contributor() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        let amount = U256::from(1_000_000u64);
        deliver_proceeds(&s, amount);

        for (start, len) in [(0_u32, 256_u32), (256, 44)] {
            runtime::pay_contributor_batch(
                &s,
                WWD,
                start,
                &abi_leaves(&leaves[start as usize..(start + len) as usize]),
                &contributor_range_proof(&leaves, start),
            )
            .unwrap_or_else(|e| panic!("batch at {start} must pay: {e:?}"));
        }

        let total = nominal_total(&leaves);
        for leaf in &leaves {
            let expected = amount * leaf.nominal / total;
            assert_eq!(
                s.balance(leaf.owner).unwrap(),
                expected,
                "owner {:?} share",
                leaf.owner
            );
        }

        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.paid_leaf_count, 300);
        // The last batch closed the round and burned what floor division left,
        // so nothing of this day remains on the precompile.
        assert!(round.paid_so_far < amount, "floor shares must fall short");
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });
}

#[test]
fn an_unfinished_round_keeps_the_outstanding_shares() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        let amount = U256::from(1_000_000u64);
        deliver_proceeds(&s, amount);

        // Only the first chunk is paid: the balance still owes the tail its
        // shares, so nothing may be burned yet.
        runtime::pay_contributor_batch(
            &s,
            WWD,
            0,
            &abi_leaves(&leaves[0..256]),
            &contributor_range_proof(&leaves, 0),
        )
        .unwrap();

        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.paid_leaf_count, 256);
        assert_eq!(
            s.balance(INTEX_FACTORY_ADDRESS).unwrap(),
            amount - round.paid_so_far
        );
    });
}

#[test]
fn proceeds_arriving_after_the_round_opened_are_burned() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        let amount = U256::from(1_000u64);
        deliver_proceeds(&s, amount);

        // A second chain finally delivers, long after the fan-in window closed.
        let late = U256::from(400u64);
        s.increase_balance(INTEX_FACTORY_ADDRESS, late).unwrap();
        runtime::distribute(&s, ORIGIN_ROUTER_ADDRESS, WWD, CHAIN + 1, late).unwrap();

        // The round still distributes only what it froze, and the late delivery
        // is destroyed rather than left on the balance.
        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.amount, amount);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), amount);
    });
}

#[test]
fn an_understated_nominal_total_cannot_spend_another_days_proceeds() {
    with_factory(|s| {
        let leaves = population(300);
        // The certified total is off-chain input and is never reconciled against
        // the leaves under the root, so a halved one inflates every share.
        install_generation_with_total(&s, &leaves, nominal_total(&leaves) / U256::from(2u64));
        let amount = U256::from(100_000u64);
        deliver_proceeds(&s, amount);

        // Another day's money sits on the same balance and must stay there.
        let other_day = U256::from(1_000_000u64);
        s.increase_balance(INTEX_FACTORY_ADDRESS, other_day)
            .unwrap();

        let err = runtime::pay_contributor_batch(
            &s,
            WWD,
            0,
            &abi_leaves(&leaves[0..256]),
            &contributor_range_proof(&leaves, 0),
        )
        .unwrap_err();

        assert!(
            format!("{err:?}").contains("exceed the round amount"),
            "{err:?}"
        );
        assert_eq!(s.balance(leaves[0].owner).unwrap(), U256::ZERO);
        assert_eq!(
            s.balance(INTEX_FACTORY_ADDRESS).unwrap(),
            amount + other_day
        );
    });
}

#[test]
fn paying_the_same_batch_twice_reverts_before_verification() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        deliver_proceeds(&s, U256::from(1_000_000u64));

        let batch = abi_leaves(&leaves[0..256]);
        let proof = contributor_range_proof(&leaves, 0);
        runtime::pay_contributor_batch(&s, WWD, 0, &batch, &proof).unwrap();
        let paid_once = s.balance(leaves[0].owner).unwrap();

        // A garbage proof proves the bitmap gate runs first: the batch is
        // rejected as already paid, never as a bad proof.
        let err = runtime::pay_contributor_batch(&s, WWD, 0, &batch, &[B256::ZERO; 2]).unwrap_err();
        assert!(format!("{err:?}").contains("already paid"), "{err:?}");
        assert_eq!(s.balance(leaves[0].owner).unwrap(), paid_once);
    });
}

#[test]
fn batch_without_an_open_round_is_rejected() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);

        let err = runtime::pay_contributor_batch(
            &s,
            WWD,
            0,
            &abi_leaves(&leaves[0..256]),
            &contributor_range_proof(&leaves, 0),
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("no open certified"), "{err:?}");
    });
}

#[test]
fn certified_day_without_contributors_burns_the_pot() {
    with_factory(|s| {
        install_empty_generation(&s);
        deliver_proceeds(&s, U256::from(1_000u64));

        assert!(outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .is_none());
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });
}

#[test]
fn day_without_contributor_authority_holds_the_pot_until_the_deadline() {
    with_factory(|s| {
        let amount = U256::from(1_000u64);
        // Fan-in completes immediately, but no root has landed yet.
        deliver_proceeds(&s, amount);

        // Nothing is burned while the window is still open.
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), amount);
        assert!(outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .is_none());

        // The root arrives late; the preserved pot now funds the round.
        let leaves = population(300);
        install_generation(&s, &leaves);
        runtime::try_settle_proceeds(&s, WWD, DEADLINE_FUTURE - 1).unwrap();

        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .expect("late root must still open a round");
        assert_eq!(round.amount, amount);
    });
}

#[test]
fn legacy_day_keeps_using_the_cursor_path() {
    with_factory(|s| {
        let owners = [super::contrib(1), super::contrib(2)];
        outbe_intex::api::record_contributors(
            &s,
            WWD,
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(100u64)),
            ],
        )
        .unwrap();
        deliver_proceeds(&s, U256::from(1_000u64));

        // Legacy authority opens a paginated distribution, not a certified round.
        assert!(outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .is_none());
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        runtime::drain_distributions(&s).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(500u64));
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(500u64));
    });
}
