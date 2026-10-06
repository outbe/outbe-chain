use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    intent::DayType,
    receipts::{desis_request_brief_hash, LimitSplitDestination, RequestLimitSplitReceiptV1},
};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};
use outbe_promislimit::PromisLimitContract;

use crate::errors::MetadosisError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RequestLimitEffect {
    pub protocol_bundle_hash: B256,
    pub wwd: u32,
    pub pending_nonce: u64,
    pub day_type: DayType,
    pub day_limit: U256,
    pub lysis_limit_minor: U256,
    pub nominal_total: U256,
    pub logical_anchor: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RequestLimitSplit {
    /// The day's own emission plus what it drew from the accumulator.
    pub day_limit: U256,
    /// Maximum Lysis capacity, in protocol units (1e6 per whole COEN).
    pub lysis_limit_minor: U256,
    /// Maximum Desis capacity, not actual Intex issuance, in protocol units.
    pub desis_limit_minor: U256,
    /// What Lysis left of the day's own emission, credited before the auction draws.
    pub carry_over_credit: U256,
}

impl RequestLimitSplit {
    /// The day's own emission bounds Lysis, and the accumulator receives the credit for what
    /// Lysis leaves. The auction then draws from that accumulator: no more than the nominal beyond
    /// the symbolic share, and no more than the accumulator holds.
    pub(crate) fn derive(
        base_limit: U256,
        lysis_limit_minor: U256,
        nominal_total: U256,
        carry_over_before: U256,
        green: bool,
    ) -> Result<Self> {
        let invalid = || MetadosisError::InvalidOcompLimitSplit {
            day_limit: base_limit,
            lysis_limit_minor,
        };
        let carry_over_credit = base_limit
            .checked_sub(lysis_limit_minor)
            .ok_or_else(invalid)?;
        let desis_limit_minor = crate::settlement::desis_limit(
            nominal_total,
            lysis_limit_minor,
            base_limit,
            carry_over_before,
            green,
        )
        .ok_or_else(invalid)?;
        Self::assemble(
            base_limit,
            lysis_limit_minor,
            desis_limit_minor,
            carry_over_credit,
        )
    }

    fn assemble(
        base_limit: U256,
        lysis_limit_minor: U256,
        desis_limit_minor: U256,
        carry_over_credit: U256,
    ) -> Result<Self> {
        let invalid = || MetadosisError::InvalidOcompLimitSplit {
            day_limit: base_limit,
            lysis_limit_minor,
        };
        if lysis_limit_minor.checked_add(carry_over_credit) != Some(base_limit) {
            return Err(invalid().into());
        }
        let day_limit = base_limit
            .checked_add(desis_limit_minor)
            .ok_or_else(invalid)?;
        Ok(Self {
            day_limit,
            lysis_limit_minor,
            desis_limit_minor,
            carry_over_credit,
        })
    }
}

/// Apply the request effect for a split that the authoritative Metadosis job
/// state has proven fresh.
///
/// The single-attempt FSM invokes this exactly once for a WorldwideDay. An
/// existing receipt is an invariant violation rather than a replay path.
pub(crate) fn apply_fresh_request_limit_effect(
    storage: StorageHandle<'_>,
    request: RequestLimitEffect,
) -> Result<RequestLimitSplitReceiptV1> {
    let green = request.day_type == DayType::Green;
    let carry_over_before = PromisLimitContract::new(storage.clone()).get_total_unallocated()?;
    let split = RequestLimitSplit::derive(
        request.day_limit,
        request.lysis_limit_minor,
        request.nominal_total,
        carry_over_before,
        green,
    )?;
    let receipt = expected_receipt(&request, split, request.pending_nonce)?;
    receipt
        .validate_semantics()
        .map_err(protocol_error_to_revert)?;
    // The brief waits for the Lysis deadline: a day whose Lysis never completes opens no auction.
    if !split.carry_over_credit.is_zero() {
        let delta = PromisLimitContract::new(storage.clone())
            .checked_add_carry_over(split.carry_over_credit)?;
        if delta.credited != split.carry_over_credit {
            return Err(MetadosisError::OcompLimitReceiptMismatch.into());
        }
    }
    reserve_desis_limit(storage, &receipt)?;
    Ok(receipt)
}

/// Take the day's Desis Limit out of the accumulator, so later requests size their auctions
/// without it. A shortfall fails the day rather than reserving less than the receipt promises.
pub(crate) fn reserve_desis_limit(
    storage: StorageHandle<'_>,
    receipt: &RequestLimitSplitReceiptV1,
) -> Result<()> {
    let draw = desis_reservation(receipt)?;
    if draw.is_zero() {
        return Ok(());
    }
    let mut promis_limit = PromisLimitContract::new(storage);
    if promis_limit.checked_take_carry_over(draw)?.is_none() {
        return Err(MetadosisError::DesisLimitUnavailable {
            desis_limit_minor: draw,
            available: promis_limit.get_total_unallocated()?,
        }
        .into());
    }
    Ok(())
}

/// Brief Desis with the Desis Limit the request reserved.
///
/// Called once Lysis has closed, so a day whose Lysis never completed never opens an auction.
pub(crate) fn apply_auction_brief(
    storage: StorageHandle<'_>,
    receipt: &RequestLimitSplitReceiptV1,
) -> Result<()> {
    let green = receipt.day_type == DayType::Green;
    desis_reservation(receipt)?;
    storage.with_checkpoint(|| {
        let actual = outbe_desis::ocomp_limits::apply_request_desis_limit(
            storage.clone(),
            receipt.protocol_bundle_hash,
            receipt.wwd.into(),
            receipt.desis_limit_minor,
            receipt.logical_anchor,
            green,
        )?;
        if receipt.desis_brief_hash != Some(actual) {
            return Err(MetadosisError::OcompDesisBriefHashMismatch.into());
        }
        Ok(())
    })
}

/// What the request reserves from the accumulator for the auction. A red day opens no auction,
/// so this function rejects a receipt that gives it a Desis Limit.
pub(crate) fn desis_reservation(receipt: &RequestLimitSplitReceiptV1) -> Result<U256> {
    if receipt.day_type != DayType::Green && !receipt.desis_limit_minor.is_zero() {
        return Err(MetadosisError::InvalidOcompLimitSplit {
            day_limit: receipt.day_limit,
            lysis_limit_minor: receipt.lysis_limit_minor,
        }
        .into());
    }
    Ok(receipt.desis_limit_minor)
}

/// What a request keeps outside the accumulator until its day completes or fails: its Lysis
/// Limit and the Desis Limit it reserved.
pub(crate) fn retained_request_limit(receipt: &RequestLimitSplitReceiptV1) -> Result<U256> {
    receipt
        .lysis_limit_minor
        .checked_add(desis_reservation(receipt)?)
        .ok_or_else(|| {
            MetadosisError::InvalidOcompLimitSplit {
                day_limit: receipt.day_limit,
                lysis_limit_minor: receipt.lysis_limit_minor,
            }
            .into()
        })
}

fn expected_receipt(
    request: &RequestLimitEffect,
    split: RequestLimitSplit,
    effect_nonce: u64,
) -> Result<RequestLimitSplitReceiptV1> {
    let (destination, desis_limit_minor) = match request.day_type {
        DayType::Green => (LimitSplitDestination::DesisAuction, split.desis_limit_minor),
        DayType::Red => (LimitSplitDestination::CarryOver, U256::ZERO),
    };
    let carry_over_credit = split.carry_over_credit;
    let desis_brief_hash = Some(
        desis_request_brief_hash(
            request.protocol_bundle_hash,
            request.wwd,
            desis_limit_minor,
            request.logical_anchor,
        )
        .map_err(protocol_error_to_revert)?,
    );
    let receipt = RequestLimitSplitReceiptV1 {
        protocol_bundle_hash: request.protocol_bundle_hash,
        wwd: request.wwd,
        pending_nonce: effect_nonce,
        day_type: request.day_type,
        day_limit: split.day_limit,
        lysis_limit_minor: split.lysis_limit_minor,
        desis_limit_minor: split.desis_limit_minor,
        destination,
        desis_brief_hash,
        carry_over_credit,
        logical_anchor: request.logical_anchor,
    };
    receipt
        .validate_semantics()
        .map_err(protocol_error_to_revert)?;
    Ok(receipt)
}

fn protocol_error_to_revert(error: outbe_ocomp_protocol::ProtocolError) -> PrecompileError {
    crate::errors::caller_rejection(format!("invalid OCOMP request limit receipt: {error}"))
}
