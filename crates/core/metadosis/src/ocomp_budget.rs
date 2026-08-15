use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    intent::{DayType, ReferenceEntryPriceV1},
    receipts::{desis_request_brief_hash, BudgetSplitDestination, RequestBudgetSplitReceiptV1},
};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};
use outbe_promislimit::PromisLimitContract;

use crate::errors::MetadosisError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RequestBudgetEffect {
    pub protocol_bundle_hash: B256,
    pub wwd: u32,
    pub pending_nonce: u64,
    pub day_type: DayType,
    pub day_limit: U256,
    pub lysis_budget: U256,
    pub auction_entry_prices: Vec<ReferenceEntryPriceV1>,
    pub logical_anchor: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RequestBudgetSplit {
    pub day_limit: U256,
    pub lysis_budget: U256,
    pub auction_base: U256,
}

impl RequestBudgetSplit {
    pub(crate) fn derive(day_limit: U256, lysis_budget: U256) -> Result<Self> {
        let auction_base =
            day_limit
                .checked_sub(lysis_budget)
                .ok_or(MetadosisError::InvalidOcompBudgetSplit {
                    day_limit,
                    lysis_budget,
                })?;
        let reconstructed = lysis_budget.checked_add(auction_base).ok_or(
            MetadosisError::InvalidOcompBudgetSplit {
                day_limit,
                lysis_budget,
            },
        )?;
        if reconstructed != day_limit {
            return Err(MetadosisError::InvalidOcompBudgetSplit {
                day_limit,
                lysis_budget,
            }
            .into());
        }
        Ok(Self {
            day_limit,
            lysis_budget,
            auction_base,
        })
    }
}

/// Apply the request effect for a split that the authoritative Metadosis job
/// state has proven fresh.
///
/// This function intentionally does not perform receipt lookup. OCM-08 owns
/// that lookup and must call [`validate_replayed_request_budget_effect`] when
/// the effect receipt already exists.
pub(crate) fn apply_fresh_request_budget_effect(
    storage: StorageHandle<'_>,
    request: RequestBudgetEffect,
) -> Result<RequestBudgetSplitReceiptV1> {
    let split = RequestBudgetSplit::derive(request.day_limit, request.lysis_budget)?;
    let receipt = expected_receipt(&request, split, request.pending_nonce)?;
    let green = request.day_type == DayType::Green;
    // One checkpoint: a red day writes both the brief and the carry-over, and
    // neither may survive the other's failure.
    storage.with_checkpoint(|| {
        let actual = outbe_desis::ocomp_budget::apply_request_auction_base(
            storage.clone(),
            request.protocol_bundle_hash,
            request.wwd.into(),
            split.auction_base,
            &request.auction_entry_prices,
            request.logical_anchor,
            green,
        )?;
        if receipt.desis_brief_hash != Some(actual) {
            return Err(MetadosisError::OcompDesisBriefHashMismatch.into());
        }
        if !green {
            let delta = PromisLimitContract::new(storage.clone())
                .checked_add_carry_over(split.auction_base)?;
            if delta.credited != receipt.carry_over_credit {
                return Err(MetadosisError::OcompBudgetReceiptMismatch.into());
            }
        }
        Ok(())
    })?;
    receipt
        .validate_semantics()
        .map_err(protocol_error_to_revert)?;
    Ok(receipt)
}

/// Validate an authoritative receipt for a replay without touching owner
/// storage. OCM-08 decides between fresh apply and replay from persisted job
/// state; callers cannot accidentally select the replay path with `None`.
pub(crate) fn validate_replayed_request_budget_effect(
    request: RequestBudgetEffect,
    existing: &RequestBudgetSplitReceiptV1,
) -> Result<RequestBudgetSplitReceiptV1> {
    if existing.pending_nonce > request.pending_nonce {
        return Err(MetadosisError::OcompBudgetEffectFromFuture {
            effect_nonce: existing.pending_nonce,
            current_nonce: request.pending_nonce,
        }
        .into());
    }
    let split = RequestBudgetSplit::derive(request.day_limit, request.lysis_budget)?;
    let expected = expected_receipt(&request, split, existing.pending_nonce)?;
    if existing != &expected {
        return Err(MetadosisError::OcompBudgetReceiptMismatch.into());
    }
    Ok(existing.clone())
}

fn expected_receipt(
    request: &RequestBudgetEffect,
    split: RequestBudgetSplit,
    effect_nonce: u64,
) -> Result<RequestBudgetSplitReceiptV1> {
    let (destination, briefed_supply, carry_over_credit) = match request.day_type {
        DayType::Green => (
            BudgetSplitDestination::DesisAuction,
            split.auction_base,
            U256::ZERO,
        ),
        DayType::Red => (
            BudgetSplitDestination::CarryOver,
            U256::ZERO,
            split.auction_base,
        ),
    };
    let desis_brief_hash = Some(
        desis_request_brief_hash(
            request.protocol_bundle_hash,
            request.wwd,
            briefed_supply,
            &request.auction_entry_prices,
            request.logical_anchor,
        )
        .map_err(protocol_error_to_revert)?,
    );
    let receipt = RequestBudgetSplitReceiptV1 {
        protocol_bundle_hash: request.protocol_bundle_hash,
        wwd: request.wwd,
        pending_nonce: effect_nonce,
        day_type: request.day_type,
        day_limit: split.day_limit,
        lysis_budget: split.lysis_budget,
        auction_base: split.auction_base,
        destination,
        desis_brief_hash,
        carry_over_credit,
        auction_entry_prices: request.auction_entry_prices.clone(),
        logical_anchor: request.logical_anchor,
    };
    receipt
        .validate_semantics()
        .map_err(protocol_error_to_revert)?;
    Ok(receipt)
}

fn protocol_error_to_revert(error: outbe_ocomp_protocol::ProtocolError) -> PrecompileError {
    crate::errors::caller_rejection(format!("invalid OCOMP request budget receipt: {error}"))
}
