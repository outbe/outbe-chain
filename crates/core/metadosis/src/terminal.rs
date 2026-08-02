use alloy_primitives::{B256, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::RetirementOutcome;
use outbe_primitives::error::{PrecompileError, Result};

use crate::constants::MAX_RETAINED_WWDS;
use crate::schema::{
    status, terminal_outcome, terminal_retirement, CapacityForfeitureReceiptState,
    MetadosisContract, WorldwideDayEntryExt, WorldwideDayTerminalReceiptState,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalReceiptValidationContext {
    status: u8,
    active_memberships: usize,
    closed_memberships: usize,
    expected_value_routed: U256,
}

impl TerminalReceiptValidationContext {
    pub(crate) const fn new(
        status: u8,
        active_memberships: usize,
        closed_memberships: usize,
        expected_value_routed: U256,
    ) -> Self {
        Self {
            status,
            active_memberships,
            closed_memberships,
            expected_value_routed,
        }
    }
}

pub(crate) fn validate_terminal_receipt_state(
    receipt: &WorldwideDayTerminalReceiptState,
    expected_outcome: u8,
    context: TerminalReceiptValidationContext,
) -> Result<()> {
    if receipt.outcome != expected_outcome
        || context.status != status::FAILED
        || context.active_memberships != 0
        || context.closed_memberships != 1
        || receipt.value_routed != context.expected_value_routed
        || receipt.carry_over_before.checked_add(receipt.value_routed)
            != Some(receipt.carry_over_after)
        || !matches!(
            receipt.retirement,
            terminal_retirement::NOT_PRESENT | terminal_retirement::REQUESTED
        )
        || receipt.block_number == 0
    {
        return Err(PrecompileError::Fatal(
            "Metadosis WWD terminal receipt is invalid".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_capacity_forfeiture_detail(
    generic: &WorldwideDayTerminalReceiptState,
    detail: &CapacityForfeitureReceiptState,
    context: TerminalReceiptValidationContext,
) -> Result<()> {
    validate_terminal_receipt_state(generic, terminal_outcome::CAPACITY_FORFEITURE, context)?;
    let max_retained_wwds = u32::try_from(MAX_RETAINED_WWDS)
        .map_err(|_| PrecompileError::Fatal("retained cap exceeds u32".into()))?;
    if detail.outcome != terminal_outcome::CAPACITY_FORFEITURE
        || detail.max_retained_wwds != max_retained_wwds
        || detail.retained_count_before != detail.max_retained_wwds
        || generic.value_routed != detail.value_routed
        || generic.carry_over_before != detail.carry_over_before
        || generic.carry_over_after != detail.carry_over_after
        || generic.retirement != detail.retirement
        || generic.block_number != detail.block_number
        || detail.source_generation != 0
        || detail.retired_generation != 1
        || match detail.retirement {
            terminal_retirement::NOT_PRESENT => {
                !detail.sealed_collection_root.is_zero()
                    || detail.forfeited_count != 0
                    || !detail.forfeited_nominal.is_zero()
            }
            terminal_retirement::REQUESTED => detail.sealed_collection_root.is_zero(),
            _ => true,
        }
    {
        return Err(PrecompileError::Fatal(
            "capacity-forfeiture detail differs from terminal receipt".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MissedOfferingReceipt {
    pub worldwide_day: WorldwideDay,
    pub value_routed: U256,
    pub carry_over_before: U256,
    pub carry_over_after: U256,
    pub retirement: RetirementOutcome,
    pub block_number: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapacityForfeitureReceipt {
    pub worldwide_day: WorldwideDay,
    pub max_retained_wwds: u32,
    pub retained_count_before: u32,
    pub value_routed: U256,
    pub carry_over_before: U256,
    pub carry_over_after: U256,
    pub sealed_collection_root: B256,
    pub forfeited_count: u32,
    pub forfeited_nominal: U256,
    pub source_generation: u64,
    pub retired_generation: u64,
    pub retirement: RetirementOutcome,
    pub block_number: u64,
}

impl MetadosisContract<'_> {
    fn terminal_receipt_validation_context(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<TerminalReceiptValidationContext> {
        let active = self.active_wwd.read_all()?;
        let closed = self.closed_wwd.read_all()?;
        Ok(TerminalReceiptValidationContext::new(
            self.worldwide_days.entry(worldwide_day).status().read()?,
            active
                .iter()
                .filter(|candidate| **candidate == worldwide_day)
                .count(),
            closed
                .iter()
                .filter(|candidate| **candidate == worldwide_day)
                .count(),
            self.worldwide_days
                .entry(worldwide_day)
                .metadosis_limit_amount()
                .read()?,
        ))
    }

    pub(crate) fn write_missed_offering_receipt(
        &mut self,
        receipt: MissedOfferingReceipt,
    ) -> Result<()> {
        if self
            .worldwide_day_terminal_receipts
            .get(receipt.worldwide_day)?
            .is_some()
        {
            return Err(PrecompileError::Fatal(
                "Metadosis WWD terminal receipt is immutable".into(),
            ));
        }
        self.worldwide_day_terminal_receipts
            .create(&WorldwideDayTerminalReceiptState {
                wwd: receipt.worldwide_day,
                outcome: terminal_outcome::MISSED_OFFERING,
                value_routed: receipt.value_routed,
                carry_over_before: receipt.carry_over_before,
                carry_over_after: receipt.carry_over_after,
                retirement: encode_retirement(receipt.retirement),
                block_number: receipt.block_number,
            })
    }

    pub(crate) fn read_missed_offering_receipt(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<MissedOfferingReceipt>> {
        let stored = self.worldwide_day_terminal_receipts.get(worldwide_day)?;
        let capacity_detail = self.capacity_forfeiture_receipts.get(worldwide_day)?;
        let Some(stored) = stored else {
            if capacity_detail.is_some() {
                return Err(PrecompileError::Fatal(
                    "capacity-forfeiture detail has no generic terminal receipt".into(),
                ));
            }
            return Ok(None);
        };
        if let Some(capacity_detail) = capacity_detail {
            if stored.outcome == terminal_outcome::CAPACITY_FORFEITURE {
                validate_capacity_forfeiture_detail(
                    &stored,
                    &capacity_detail,
                    self.terminal_receipt_validation_context(worldwide_day)?,
                )?;
                return Ok(None);
            }
            return Err(PrecompileError::Fatal(
                "missed-offering receipt has capacity-forfeiture detail".into(),
            ));
        }
        validate_terminal_receipt_state(
            &stored,
            terminal_outcome::MISSED_OFFERING,
            self.terminal_receipt_validation_context(worldwide_day)?,
        )?;
        Ok(Some(MissedOfferingReceipt {
            worldwide_day,
            value_routed: stored.value_routed,
            carry_over_before: stored.carry_over_before,
            carry_over_after: stored.carry_over_after,
            retirement: decode_retirement(stored.retirement)?,
            block_number: stored.block_number,
        }))
    }

    pub(crate) fn write_capacity_forfeiture_receipt(
        &mut self,
        receipt: CapacityForfeitureReceipt,
    ) -> Result<()> {
        if self
            .worldwide_day_terminal_receipts
            .get(receipt.worldwide_day)?
            .is_some()
            || self
                .capacity_forfeiture_receipts
                .get(receipt.worldwide_day)?
                .is_some()
        {
            return Err(PrecompileError::Fatal(
                "Metadosis capacity-forfeiture receipt is immutable".into(),
            ));
        }
        let retirement = encode_retirement(receipt.retirement);
        self.worldwide_day_terminal_receipts
            .create(&WorldwideDayTerminalReceiptState {
                wwd: receipt.worldwide_day,
                outcome: terminal_outcome::CAPACITY_FORFEITURE,
                value_routed: receipt.value_routed,
                carry_over_before: receipt.carry_over_before,
                carry_over_after: receipt.carry_over_after,
                retirement,
                block_number: receipt.block_number,
            })?;
        self.capacity_forfeiture_receipts
            .create(&CapacityForfeitureReceiptState {
                wwd: receipt.worldwide_day,
                outcome: terminal_outcome::CAPACITY_FORFEITURE,
                max_retained_wwds: receipt.max_retained_wwds,
                retained_count_before: receipt.retained_count_before,
                value_routed: receipt.value_routed,
                carry_over_before: receipt.carry_over_before,
                carry_over_after: receipt.carry_over_after,
                sealed_collection_root: receipt.sealed_collection_root,
                forfeited_count: receipt.forfeited_count,
                forfeited_nominal: receipt.forfeited_nominal,
                source_generation: receipt.source_generation,
                retired_generation: receipt.retired_generation,
                retirement,
                block_number: receipt.block_number,
            })
    }

    pub(crate) fn read_capacity_forfeiture_receipt(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<CapacityForfeitureReceipt>> {
        let generic = self.worldwide_day_terminal_receipts.get(worldwide_day)?;
        let stored = self.capacity_forfeiture_receipts.get(worldwide_day)?;
        let (generic, stored) = match (generic, stored) {
            (None, None) => return Ok(None),
            (None, Some(_)) => {
                return Err(PrecompileError::Fatal(
                    "capacity-forfeiture detail has no generic terminal receipt".into(),
                ));
            }
            (Some(generic), None) if generic.outcome == terminal_outcome::MISSED_OFFERING => {
                validate_terminal_receipt_state(
                    &generic,
                    terminal_outcome::MISSED_OFFERING,
                    self.terminal_receipt_validation_context(worldwide_day)?,
                )?;
                return Ok(None);
            }
            (Some(_), None) => {
                return Err(PrecompileError::Fatal(
                    "capacity-forfeiture terminal receipt has no detail".into(),
                ));
            }
            (Some(generic), Some(stored)) => (generic, stored),
        };
        validate_capacity_forfeiture_detail(
            &generic,
            &stored,
            self.terminal_receipt_validation_context(worldwide_day)?,
        )?;
        Ok(Some(CapacityForfeitureReceipt {
            worldwide_day,
            max_retained_wwds: stored.max_retained_wwds,
            retained_count_before: stored.retained_count_before,
            value_routed: stored.value_routed,
            carry_over_before: stored.carry_over_before,
            carry_over_after: stored.carry_over_after,
            sealed_collection_root: stored.sealed_collection_root,
            forfeited_count: stored.forfeited_count,
            forfeited_nominal: stored.forfeited_nominal,
            source_generation: stored.source_generation,
            retired_generation: stored.retired_generation,
            retirement: decode_retirement(stored.retirement)?,
            block_number: stored.block_number,
        }))
    }
}

const fn encode_retirement(outcome: RetirementOutcome) -> u8 {
    match outcome {
        RetirementOutcome::NotPresent => terminal_retirement::NOT_PRESENT,
        RetirementOutcome::Requested => terminal_retirement::REQUESTED,
    }
}

fn decode_retirement(value: u8) -> Result<RetirementOutcome> {
    match value {
        terminal_retirement::NOT_PRESENT => Ok(RetirementOutcome::NotPresent),
        terminal_retirement::REQUESTED => Ok(RetirementOutcome::Requested),
        _ => Err(PrecompileError::Fatal(
            "Metadosis WWD terminal receipt has invalid retirement outcome".into(),
        )),
    }
}
