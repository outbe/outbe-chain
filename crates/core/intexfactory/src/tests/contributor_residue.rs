//! Public-seam coverage for the certified-pot remainder.
//!
//! Floor shares stay on every leaf. Certified leaf 0 receives the leftover in
//! the same checkpoint that pays the last unpaid leaf. A legacy round whose
//! index-0 batch was paid before that owner was stored must recover it from
//! the chunk proof; recovery itself transfers nothing.

use alloy_sol_types::{SolCall, SolEvent};
use outbe_intex::payout::test_support::{contributor_leaf, contributor_range_proof};
use outbe_intex::payout::ContributorLeafData;
use outbe_intex::schema::RESIDUE_RULE_PAY_LEAF_ZERO;
use outbe_intex::{CertifiedPayoutRound, IntexContract};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;

use super::creator_reward::{
    abi_leaves, deliver_proceeds, install_generation, install_generation_with_total, nominal_total,
    population, WWD,
};
use super::*;

use crate::precompile::dispatch;

fn finish_factory(f: impl FnOnce(StorageHandle<'_>)) -> HashMapStorageProvider {
    let mut storage = super::factory_provider();
    StorageHandle::enter(&mut storage, |handle| {
        super::select_prod_profile(&handle);
        f(handle);
    });
    storage
}

fn index_zero_len(leaves: &[ContributorLeafData]) -> u32 {
    u32::try_from(leaves.len().min(256)).expect("chunk length")
}

fn pay(
    storage: &StorageHandle<'_>,
    leaves: &[ContributorLeafData],
    start: u32,
    len: u32,
) -> Result<(), PrecompileError> {
    let start_usize = start as usize;
    let batch = &leaves[start_usize..start_usize + len as usize];
    let data = IIntexFactory::payContributorBatchCall {
        worldwideDay: WWD,
        startIndex: start,
        leaves: abi_leaves(batch),
        proof: contributor_range_proof(leaves, start),
    }
    .abi_encode();
    dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).map(|_| ())
}

fn recover(
    storage: &StorageHandle<'_>,
    leaves: &[ContributorLeafData],
) -> Result<(), PrecompileError> {
    let len = index_zero_len(leaves) as usize;
    let data = IIntexFactory::recordContributorResidueRecipientCall {
        worldwideDay: WWD,
        leaves: abi_leaves(&leaves[..len]),
        proof: contributor_range_proof(leaves, 0),
    }
    .abi_encode();
    dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).map(|_| ())
}

/// Floor shares plus the undistributed remainder on leaf 0.
fn expected_payout(leaves: &[ContributorLeafData], amount: U256) -> Vec<U256> {
    let total = nominal_total(leaves);
    let mut shares: Vec<U256> = if total.is_zero() {
        vec![U256::ZERO; leaves.len()]
    } else {
        leaves
            .iter()
            .map(|leaf| amount * leaf.nominal / total)
            .collect()
    };
    let floor_sum = shares.iter().fold(U256::ZERO, |acc, share| acc + *share);
    shares[0] += amount - floor_sum;
    shares
}

fn assert_balances(storage: &StorageHandle<'_>, leaves: &[ContributorLeafData], expected: &[U256]) {
    let mut sum = U256::ZERO;
    for (leaf, share) in leaves.iter().zip(expected) {
        let balance = storage.balance(leaf.owner).unwrap();
        assert_eq!(balance, *share, "owner {:?}", leaf.owner);
        sum += balance;
    }
    assert_eq!(
        sum,
        expected.iter().fold(U256::ZERO, |acc, share| acc + *share)
    );
}

fn pay_order(storage: &StorageHandle<'_>, leaves: &[ContributorLeafData], order: &[(u32, u32)]) {
    for (start, len) in order {
        pay(storage, leaves, *start, *len).unwrap_or_else(|err| {
            panic!("batch at {start} must pay: {err:?}");
        });
    }
}

fn open_population(
    storage: &StorageHandle<'_>,
    count: u32,
    amount: U256,
) -> Vec<ContributorLeafData> {
    let leaves = population(count);
    install_generation(storage, &leaves);
    deliver_proceeds(storage, amount);
    leaves
}

#[test]
fn public_batch_pays_exact_pot_and_rejects_replay() {
    let leaves = [contributor_leaf(0, 1), contributor_leaf(1, 1)];
    let amount = U256::from(3u64);
    let data = IIntexFactory::payContributorBatchCall {
        worldwideDay: WWD,
        startIndex: 0,
        leaves: abi_leaves(&leaves),
        proof: contributor_range_proof(&leaves, 0),
    }
    .abi_encode();
    let provider = finish_factory(|s| {
        install_generation(&s, &leaves);
        deliver_proceeds(&s, amount);
        let funded = dispatch(s.clone(), &data, Address::ZERO, U256::from(1u64));
        assert!(
            matches!(
                funded,
                Err(PrecompileError::Revert(ref message))
                    if message == "non-payable function called with value"
            ),
            "{funded:?}"
        );
        dispatch(s.clone(), &data, Address::ZERO, U256::ZERO).unwrap();
        assert_eq!(s.balance(leaves[0].owner).unwrap(), U256::from(2u64));
        assert_eq!(s.balance(leaves[1].owner).unwrap(), U256::from(1u64));
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.paid_so_far, amount);
        assert_eq!(round.residue_recipient, leaves[0].owner);
        assert_eq!(round.residue_recipient_set, 1);
        assert_eq!(round.residue_rule, RESIDUE_RULE_PAY_LEAF_ZERO);
        // The same proof after close records nothing again and moves no balance.
        recover(&s, &leaves).unwrap();
        assert_eq!(s.balance(leaves[0].owner).unwrap(), U256::from(2u64));
        let replay = dispatch(s.clone(), &data, Address::ZERO, U256::ZERO).unwrap_err();
        assert!(format!("{replay:?}").contains("already paid"), "{replay:?}");
        assert_eq!(s.balance(leaves[0].owner).unwrap(), U256::from(2u64));
        assert_eq!(s.balance(leaves[1].owner).unwrap(), U256::from(1u64));
    });
    let sig = IIntexFactory::ContributorRoundClosed::SIGNATURE_HASH;
    let found = provider
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .any(|log| {
            log.topics().first() == Some(&sig)
                && IIntexFactory::ContributorRoundClosed::decode_log_data(log)
                    .map(|ev| ev.paidAmount == amount && ev.burnedAmount.is_zero())
                    .unwrap_or(false)
        });
    assert!(
        found,
        "closed event must include the remainder and burn nothing"
    );
}

#[test]
fn tail_before_head_pays_the_same_leaf_zero() {
    with_factory(|s| {
        let amount = U256::from(1_000_000u64);
        let leaves = open_population(&s, 300, amount);
        let err = recover(&s, &leaves).unwrap_err();
        assert!(format!("{err:?}").contains("leaf 0 is unpaid"), "{err:?}");
        assert_eq!(s.balance(leaves[0].owner).unwrap(), U256::ZERO);
        pay_order(&s, &leaves, &[(256, 44), (0, 256)]);
        assert_balances(&s, &leaves, &expected_payout(&leaves, amount));
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.paid_so_far, amount);
        assert_eq!(round.residue_recipient, leaves[0].owner);
    });
}

#[test]
fn head_before_tail_pays_the_same_leaf_zero() {
    with_factory(|s| {
        let amount = U256::from(1_000_000u64);
        let leaves = open_population(&s, 300, amount);
        pay_order(&s, &leaves, &[(0, 256), (256, 44)]);
        assert_balances(&s, &leaves, &expected_payout(&leaves, amount));
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.paid_so_far, amount);
        assert_eq!(round.residue_recipient, leaves[0].owner);
        assert_eq!(round.residue_rule, RESIDUE_RULE_PAY_LEAF_ZERO);
    });
}

fn mark_paid_prefix(storage: &StorageHandle<'_>, leaf_count: u32) {
    let intex = IntexContract::new(storage.clone());
    let full_words = leaf_count / 256;
    let remainder = leaf_count % 256;
    for word in 0..full_words {
        intex
            .ocomp_paid_leaves
            .write(&IntexContract::paid_bitmap_key(WWD, word), U256::MAX)
            .unwrap();
    }
    if remainder != 0 {
        let mask = (U256::from(1u8) << remainder) - U256::from(1u8);
        intex
            .ocomp_paid_leaves
            .write(&IntexContract::paid_bitmap_key(WWD, full_words), mask)
            .unwrap();
    }
}

fn credit_floors(
    storage: &StorageHandle<'_>,
    leaves: &[ContributorLeafData],
    total: U256,
    amount: U256,
) -> U256 {
    let mut paid = U256::ZERO;
    for leaf in leaves {
        let share = amount * leaf.nominal / total;
        paid += share;
        storage.increase_balance(leaf.owner, share).unwrap();
    }
    paid
}

fn write_legacy_round(
    storage: &StorageHandle<'_>,
    amount: U256,
    paid_so_far: U256,
    paid_leaf_count: u32,
) {
    IntexContract::new(storage.clone())
        .ocomp_payout_round
        .create(&CertifiedPayoutRound {
            wwd: WWD,
            amount,
            paid_so_far,
            paid_leaf_count,
            active: 1,
            residue_recipient: Address::ZERO,
            residue_recipient_set: 0,
            residue_rule: 0,
        })
        .unwrap();
}

#[test]
fn legacy_recovery_then_tail_pays_the_residue_once() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        let amount = U256::from(1_000_000u64);
        let total = nominal_total(&leaves);
        let head_paid = credit_floors(&s, &leaves[..256], total, amount);
        s.increase_balance(INTEX_FACTORY_ADDRESS, amount - head_paid)
            .unwrap();
        write_legacy_round(&s, amount, head_paid, 256);
        mark_paid_prefix(&s, 256);
        let leaf_zero_before = s.balance(leaves[0].owner).unwrap();
        let factory_before = s.balance(INTEX_FACTORY_ADDRESS).unwrap();

        recover(&s, &leaves).unwrap();
        assert_eq!(s.balance(leaves[0].owner).unwrap(), leaf_zero_before);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), factory_before);
        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.residue_recipient, leaves[0].owner);
        assert_eq!(round.residue_recipient_set, 1);
        assert_eq!(round.residue_rule, 0);
        // A second recovery is a no-op.
        recover(&s, &leaves).unwrap();
        assert_eq!(s.balance(leaves[0].owner).unwrap(), leaf_zero_before);

        pay(&s, &leaves, 256, 44).unwrap();
        let expected = expected_payout(&leaves, amount);
        assert_eq!(
            s.balance(leaves[0].owner).unwrap() - leaf_zero_before,
            expected[0] - (amount * leaves[0].nominal / nominal_total(&leaves))
        );
        assert_balances(&s, &leaves, &expected);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        let closed = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(closed.paid_so_far, amount);
        assert_eq!(closed.paid_leaf_count, 300);
    });
}

#[test]
fn legacy_closed_round_recovery_moves_nothing() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        let amount = U256::from(1_000_000u64);
        let paid = credit_floors(&s, &leaves, nominal_total(&leaves), amount);
        assert!(paid < amount, "this fixture must leave a burned remainder");
        let other_day = U256::from(50u64);
        s.increase_balance(INTEX_FACTORY_ADDRESS, other_day)
            .unwrap();
        write_legacy_round(&s, amount, paid, 300);
        mark_paid_prefix(&s, 300);
        let before = s.balance(leaves[0].owner).unwrap();

        let err = recover(&s, &leaves).unwrap_err();
        assert!(format!("{err:?}").contains("already closed"), "{err:?}");
        assert_eq!(s.balance(leaves[0].owner).unwrap(), before);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), other_day);
        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.residue_recipient_set, 0);
        assert_eq!(round.paid_so_far, paid);
    });
}

#[test]
fn legacy_tail_without_recovery_rolls_back() {
    with_factory(|s| {
        let leaves = population(300);
        install_generation(&s, &leaves);
        let amount = U256::from(1_000_000u64);
        let total = nominal_total(&leaves);
        let head_paid = credit_floors(&s, &leaves[..256], total, amount);
        s.increase_balance(INTEX_FACTORY_ADDRESS, amount - head_paid)
            .unwrap();
        write_legacy_round(&s, amount, head_paid, 256);
        mark_paid_prefix(&s, 256);
        let factory_before = s.balance(INTEX_FACTORY_ADDRESS).unwrap();
        let leaf_zero_before = s.balance(leaves[0].owner).unwrap();

        let err = pay(&s, &leaves, 256, 44).unwrap_err();
        assert!(
            format!("{err:?}").contains("residue recipient is not recorded"),
            "{err:?}"
        );
        assert_eq!(s.balance(leaves[256].owner).unwrap(), U256::ZERO);
        assert_eq!(s.balance(leaves[0].owner).unwrap(), leaf_zero_before);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), factory_before);
        assert_eq!(
            outbe_intex::api::paid_leaves_word(&s, WWD, 1).unwrap(),
            U256::ZERO
        );
        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.paid_leaf_count, 256);
        assert_eq!(round.paid_so_far, head_paid);
        assert_eq!(round.residue_recipient_set, 0);
    });
}

#[test]
fn recovery_rejects_a_different_owner() {
    with_factory(|s| {
        let leaves = [contributor_leaf(0, 1), contributor_leaf(1, 1)];
        install_generation(&s, &leaves);
        deliver_proceeds(&s, U256::from(3u64));
        pay(&s, &leaves, 0, 2).unwrap();
        let mut round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        round.residue_recipient = Address::repeat_byte(0x11);
        IntexContract::new(s.clone())
            .ocomp_payout_round
            .update(&round)
            .unwrap();
        let err = recover(&s, &leaves).unwrap_err();
        assert!(format!("{err:?}").contains("conflicts"), "{err:?}");
        let stored = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(stored.residue_recipient, Address::repeat_byte(0x11));
        assert_eq!(s.balance(leaves[0].owner).unwrap(), U256::from(2u64));
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });
}

#[test]
fn zero_nominal_leaf_zero_receives_the_remainder() {
    with_factory(|s| {
        let leaves = [
            contributor_leaf(0, 0),
            contributor_leaf(1, 1),
            contributor_leaf(2, 1),
        ];
        let amount = U256::from(3u64);
        install_generation(&s, &leaves);
        deliver_proceeds(&s, amount);
        pay(&s, &leaves, 0, 3).unwrap();
        assert_balances(&s, &leaves, &expected_payout(&leaves, amount));
        assert_eq!(s.balance(leaves[0].owner).unwrap(), U256::from(1u64));
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.paid_so_far, amount);
        assert_eq!(round.residue_recipient, leaves[0].owner);
    });
}

#[test]
fn all_zero_nominals_pay_the_whole_pot_to_leaf_zero() {
    with_factory(|s| {
        let leaves = [contributor_leaf(0, 0), contributor_leaf(1, 0)];
        let amount = U256::from(5u64);
        install_generation(&s, &leaves);
        deliver_proceeds(&s, amount);
        pay(&s, &leaves, 0, 2).unwrap();
        assert_eq!(s.balance(leaves[0].owner).unwrap(), amount);
        assert_eq!(s.balance(leaves[1].owner).unwrap(), U256::ZERO);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });
}

#[test]
fn zero_total_with_a_positive_nominal_pays_nothing() {
    with_factory(|s| {
        let leaves = [contributor_leaf(0, 5)];
        install_generation_with_total(&s, &leaves, U256::ZERO);
        let amount = U256::from(9u64);
        deliver_proceeds(&s, amount);
        let err = pay(&s, &leaves, 0, 1).unwrap_err();
        assert!(
            format!("{err:?}").contains("nominal total is zero"),
            "{err:?}"
        );
        assert_eq!(s.balance(leaves[0].owner).unwrap(), U256::ZERO);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), amount);
        let round = outbe_intex::api::certified_payout_round(&s, WWD)
            .unwrap()
            .unwrap();
        assert_eq!(round.paid_leaf_count, 0);
        assert_eq!(round.paid_so_far, U256::ZERO);
        assert_eq!(round.residue_recipient_set, 0);
    });
}
