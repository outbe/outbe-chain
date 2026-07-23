use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    intent::DayType,
    receipts::{desis_request_brief_hash, BudgetSplitDestination, RequestBudgetSplitReceiptV1},
};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};
use outbe_promislimit::PromisLimitContract;

use crate::errors::MetadosisError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RequestBudgetEffect {
    pub protocol_bundle_hash: B256,
    pub wwd: u32,
    pub pending_nonce: u64,
    pub day_type: DayType,
    pub day_limit: U256,
    pub lysis_budget: U256,
    pub auction_entry_price: U256,
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

pub(crate) fn apply_request_budget_effect(
    storage: StorageHandle<'_>,
    request: RequestBudgetEffect,
    existing_receipt: Option<&RequestBudgetSplitReceiptV1>,
) -> Result<RequestBudgetSplitReceiptV1> {
    let split = RequestBudgetSplit::derive(request.day_limit, request.lysis_budget)?;
    if let Some(existing) = existing_receipt {
        if existing.pending_nonce > request.pending_nonce {
            return Err(MetadosisError::OcompBudgetEffectFromFuture {
                effect_nonce: existing.pending_nonce,
                current_nonce: request.pending_nonce,
            }
            .into());
        }
        let expected = expected_receipt(request, split, existing.pending_nonce)?;
        if existing != &expected {
            return Err(MetadosisError::OcompBudgetReceiptMismatch.into());
        }
        return Ok(existing.clone());
    }

    let receipt = expected_receipt(request, split, request.pending_nonce)?;
    storage.with_checkpoint(|| {
        match request.day_type {
            DayType::Green => {
                let actual = outbe_desis::ocomp_budget::apply_request_auction_base(
                    storage.clone(),
                    request.protocol_bundle_hash,
                    request.wwd,
                    split.auction_base,
                    request.auction_entry_price,
                    request.logical_anchor,
                )?;
                if receipt.desis_brief_hash != Some(actual) {
                    return Err(MetadosisError::OcompDesisBriefHashMismatch.into());
                }
            }
            DayType::Red => {
                let delta = PromisLimitContract::new(storage.clone())
                    .checked_add_carry_over(split.auction_base)?;
                if delta.credited != receipt.carry_over_credit {
                    return Err(MetadosisError::OcompBudgetReceiptMismatch.into());
                }
            }
        }
        receipt
            .validate_semantics()
            .map_err(protocol_error_to_revert)?;
        Ok(receipt.clone())
    })
}

fn expected_receipt(
    request: RequestBudgetEffect,
    split: RequestBudgetSplit,
    effect_nonce: u64,
) -> Result<RequestBudgetSplitReceiptV1> {
    let (destination, desis_brief_hash, carry_over_credit) = match request.day_type {
        DayType::Green => (
            BudgetSplitDestination::DesisAuction,
            Some(
                desis_request_brief_hash(
                    request.protocol_bundle_hash,
                    request.wwd,
                    split.auction_base,
                    request.auction_entry_price,
                    request.logical_anchor,
                )
                .map_err(protocol_error_to_revert)?,
            ),
            U256::ZERO,
        ),
        DayType::Red => (BudgetSplitDestination::CarryOver, None, split.auction_base),
    };
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
        auction_entry_price: request.auction_entry_price,
        logical_anchor: request.logical_anchor,
    };
    receipt
        .validate_semantics()
        .map_err(protocol_error_to_revert)?;
    Ok(receipt)
}

fn protocol_error_to_revert(error: outbe_ocomp_protocol::ProtocolError) -> PrecompileError {
    PrecompileError::Revert(format!("invalid OCOMP request budget receipt: {error}"))
}
