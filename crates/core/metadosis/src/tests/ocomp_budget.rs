use super::*;

use outbe_desis::{AuctionStage, DesisContract};
use outbe_ocomp_protocol::{
    intent::DayType,
    receipts::{desis_request_brief_hash, BudgetSplitDestination},
};
use outbe_primitives::error::PrecompileError;

use crate::ocomp_budget::{
    apply_fresh_request_budget_effect, validate_replayed_request_budget_effect,
    RequestBudgetEffect, RequestBudgetSplit,
};

#[test]
fn request_budget_split_is_exact_at_zero_max_and_rejects_over_budget() {
    assert_eq!(
        RequestBudgetSplit::derive(U256::ZERO, U256::ZERO).unwrap(),
        RequestBudgetSplit {
            day_limit: U256::ZERO,
            lysis_budget: U256::ZERO,
            auction_base: U256::ZERO,
        }
    );
    assert_eq!(
        RequestBudgetSplit::derive(U256::MAX, U256::MAX).unwrap(),
        RequestBudgetSplit {
            day_limit: U256::MAX,
            lysis_budget: U256::MAX,
            auction_base: U256::ZERO,
        }
    );
    assert_eq!(
        RequestBudgetSplit::derive(U256::MAX, U256::ZERO).unwrap(),
        RequestBudgetSplit {
            day_limit: U256::MAX,
            lysis_budget: U256::ZERO,
            auction_base: U256::MAX,
        }
    );
    assert!(matches!(
        RequestBudgetSplit::derive(U256::from(9), U256::from(10)),
        Err(PrecompileError::Revert(_))
    ));
}

#[test]
fn green_request_commits_exact_auction_base_and_canonical_receipt() {
    with_storage(|storage| {
        let request = RequestBudgetEffect {
            protocol_bundle_hash: B256::repeat_byte(0x41),
            wwd: 20_260_101,
            pending_nonce: 1,
            day_type: DayType::Green,
            day_limit: U256::from(100),
            lysis_budget: U256::from(40),
            auction_entry_price: U256::from(2),
            logical_anchor: 1_699_920_005,
        };

        let receipt = apply_fresh_request_budget_effect(storage.clone(), request)
            .expect("GREEN request budget effect");

        assert_eq!(receipt.day_limit, U256::from(100));
        assert_eq!(receipt.lysis_budget, U256::from(40));
        assert_eq!(receipt.auction_base, U256::from(60));
        assert_eq!(receipt.destination, BudgetSplitDestination::DesisAuction);
        assert_eq!(receipt.carry_over_credit, U256::ZERO);
        assert_eq!(
            receipt.desis_brief_hash,
            Some(
                desis_request_brief_hash(
                    request.protocol_bundle_hash,
                    request.wwd,
                    U256::from(60),
                    request.auction_entry_price,
                    request.logical_anchor,
                )
                .unwrap()
            )
        );

        let desis = DesisContract::new(storage.clone());
        assert_eq!(
            desis.auction_stage.read(&request.wwd.into()).unwrap(),
            AuctionStage::Briefed as u8
        );
        assert_eq!(
            desis
                .pending_supply_promis
                .read(&request.wwd.into())
                .unwrap(),
            U256::from(60)
        );
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn red_request_skips_desis_and_credits_exact_auction_base() {
    with_storage(|storage| {
        let request = RequestBudgetEffect {
            protocol_bundle_hash: B256::repeat_byte(0x41),
            wwd: 20_260_102,
            pending_nonce: 1,
            day_type: DayType::Red,
            day_limit: U256::from(100),
            lysis_budget: U256::from(40),
            auction_entry_price: U256::from(2),
            logical_anchor: 1_699_920_005,
        };

        let receipt = apply_fresh_request_budget_effect(storage.clone(), request)
            .expect("RED request budget effect");

        assert_eq!(receipt.destination, BudgetSplitDestination::CarryOver);
        assert_eq!(receipt.desis_brief_hash, None);
        assert_eq!(receipt.carry_over_credit, U256::from(60));
        let desis = DesisContract::new(storage.clone());
        assert_eq!(desis.auction_stage.read(&request.wwd.into()).unwrap(), 0);
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            U256::from(60)
        );
    });
}

#[test]
fn retry_reuses_original_receipt_without_repeating_either_request_effect() {
    with_storage(|storage| {
        let green = RequestBudgetEffect {
            protocol_bundle_hash: B256::repeat_byte(0x41),
            wwd: 20_260_103,
            pending_nonce: 1,
            day_type: DayType::Green,
            day_limit: U256::from(100),
            lysis_budget: U256::from(40),
            auction_entry_price: U256::from(2),
            logical_anchor: 1_699_920_005,
        };
        let green_receipt =
            apply_fresh_request_budget_effect(storage.clone(), green).expect("GREEN first effect");
        let green_retry = RequestBudgetEffect {
            pending_nonce: 2,
            ..green
        };
        assert_eq!(
            validate_replayed_request_budget_effect(green_retry, &green_receipt)
                .expect("GREEN retry"),
            green_receipt
        );
        let desis = DesisContract::new(storage.clone());
        assert_eq!(desis.sched_active_count.read().unwrap(), 1);
        assert_eq!(
            desis.pending_supply_promis.read(&green.wwd.into()).unwrap(),
            U256::from(60)
        );

        let red = RequestBudgetEffect {
            wwd: 20_260_104,
            day_type: DayType::Red,
            ..green
        };
        let red_receipt =
            apply_fresh_request_budget_effect(storage.clone(), red).expect("RED first effect");
        let red_retry = RequestBudgetEffect {
            pending_nonce: 2,
            ..red
        };
        assert_eq!(
            validate_replayed_request_budget_effect(red_retry, &red_receipt).expect("RED retry"),
            red_receipt
        );
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            U256::from(60)
        );
    });
}

#[test]
fn retry_rejects_tampered_or_future_receipts_without_writing() {
    with_storage(|storage| {
        let request = RequestBudgetEffect {
            protocol_bundle_hash: B256::repeat_byte(0x41),
            wwd: 20_260_107,
            pending_nonce: 1,
            day_type: DayType::Red,
            day_limit: U256::from(100),
            lysis_budget: U256::from(40),
            auction_entry_price: U256::from(2),
            logical_anchor: 1_699_920_005,
        };
        let receipt = apply_fresh_request_budget_effect(storage.clone(), request).unwrap();
        let before = PromisLimitContract::new(storage.clone())
            .get_total_unallocated()
            .unwrap();

        let mut tampered = receipt.clone();
        tampered.auction_entry_price += U256::from(1);
        assert!(matches!(
            validate_replayed_request_budget_effect(request, &tampered),
            Err(PrecompileError::Fatal(_))
        ));

        let mut future = receipt;
        future.pending_nonce = request.pending_nonce + 1;
        assert!(matches!(
            validate_replayed_request_budget_effect(request, &future),
            Err(PrecompileError::Fatal(_))
        ));
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            before
        );
    });
}

#[test]
fn strict_desis_refusal_leaves_the_existing_brief_and_carry_over_unchanged() {
    with_storage(|storage| {
        let wwd = outbe_common::WorldwideDay::new(20_260_105);
        outbe_desis::api::dispatch_auction_brief(
            storage.clone(),
            wwd,
            U256::from(7),
            vec![outbe_desis::ReferencePrice {
                iso_code: 840,
                entry_price_minor: U256::from(3),
            }],
            true,
            1_699_920_005,
        )
        .unwrap();
        PromisLimitContract::new(storage.clone())
            .checked_add_carry_over(U256::from(5))
            .unwrap();
        let request = RequestBudgetEffect {
            protocol_bundle_hash: B256::repeat_byte(0x41),
            wwd: wwd.into(),
            pending_nonce: 1,
            day_type: DayType::Green,
            day_limit: U256::from(100),
            lysis_budget: U256::from(40),
            auction_entry_price: U256::from(2),
            logical_anchor: 1_699_920_005,
        };

        assert!(apply_fresh_request_budget_effect(storage.clone(), request).is_err());

        let desis = DesisContract::new(storage.clone());
        assert_eq!(
            desis.pending_supply_promis.read(&wwd).unwrap(),
            U256::from(7)
        );
        assert_eq!(desis.sched_active_count.read().unwrap(), 1);
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            U256::from(5)
        );
    });
}

#[test]
fn red_carry_over_overflow_reverts_without_a_partial_request_effect() {
    with_storage(|storage| {
        let before = U256::MAX - U256::from(5);
        PromisLimitContract::new(storage.clone())
            .checked_add_carry_over(before)
            .unwrap();
        let request = RequestBudgetEffect {
            protocol_bundle_hash: B256::repeat_byte(0x41),
            wwd: 20_260_106,
            pending_nonce: 1,
            day_type: DayType::Red,
            day_limit: U256::from(20),
            lysis_budget: U256::from(10),
            auction_entry_price: U256::from(2),
            logical_anchor: 1_699_920_005,
        };

        assert!(apply_fresh_request_budget_effect(storage.clone(), request).is_err());
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            before
        );
        assert_eq!(
            DesisContract::new(storage)
                .auction_stage
                .read(&request.wwd.into())
                .unwrap(),
            0
        );
    });
}
