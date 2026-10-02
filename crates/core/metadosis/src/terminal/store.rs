use super::{
    codec,
    model::{CapacityForfeitureDetail, TerminalReceiptCommon, WwdTerminalReceipt},
    CapacityForfeitureReceipt, MetadosisFailureReceipt, MissedOfferingReceipt,
    TerminalReceiptValidationContext,
};
use crate::{errors::storage_corruption_message, schema::MetadosisContract};
use outbe_primitives::{error::Result, time::WorldwideDay};

impl MetadosisContract<'_> {
    pub(crate) fn read_terminal_receipt(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<WwdTerminalReceipt>> {
        let bytes = self
            .worldwide_day_terminal_receipts
            .get_bytes(&worldwide_day);
        let len = bytes.len()?;
        if len == 0 {
            return Ok(None);
        }
        if len != codec::COMMON_LEN && len != codec::CAPACITY_LEN {
            return Err(storage_corruption_message(
                "terminal receipt has non-canonical length",
            ));
        }
        codec::decode(&bytes.read()?, worldwide_day).map(Some)
    }

    fn write_terminal_receipt(&self, receipt: WwdTerminalReceipt) -> Result<()> {
        let bytes = self
            .worldwide_day_terminal_receipts
            .get_bytes(&receipt.common().worldwide_day);
        if !bytes.is_empty()? {
            return Err(storage_corruption_message(
                "Metadosis WWD terminal receipt is immutable",
            ));
        }
        bytes.write(&codec::encode(&receipt))
    }

    fn terminal_receipt_validation_context(
        &self,
        worldwide_day: WorldwideDay,
        expected_value: Option<alloy_primitives::U256>,
    ) -> Result<TerminalReceiptValidationContext> {
        use crate::schema::WorldwideDayEntryExt;
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
            match expected_value {
                Some(value) => value,
                None => self
                    .worldwide_days
                    .entry(worldwide_day)
                    .metadosis_limit_amount()
                    .read()?,
            },
        ))
    }

    pub(crate) fn write_missed_offering_receipt(
        &mut self,
        receipt: MissedOfferingReceipt,
    ) -> Result<()> {
        self.write_terminal_receipt(WwdTerminalReceipt::MissedOffering(TerminalReceiptCommon {
            worldwide_day: receipt.worldwide_day,
            value_routed: receipt.value_routed,
            carry_over_before: receipt.carry_over_before,
            carry_over_after: receipt.carry_over_after,
            retirement: receipt.retirement,
            block_number: receipt.block_number,
        }))
    }

    pub(crate) fn write_metadosis_failure_receipt(
        &mut self,
        receipt: MetadosisFailureReceipt,
    ) -> Result<()> {
        self.write_terminal_receipt(WwdTerminalReceipt::MetadosisFailure(
            TerminalReceiptCommon {
                worldwide_day: receipt.worldwide_day,
                value_routed: receipt.value_routed,
                carry_over_before: receipt.carry_over_before,
                carry_over_after: receipt.carry_over_after,
                retirement: receipt.retirement,
                block_number: receipt.block_number,
            },
        ))
    }

    pub(crate) fn read_metadosis_failure_receipt(
        &self,
        worldwide_day: WorldwideDay,
        expected_value_routed: alloy_primitives::U256,
    ) -> Result<Option<MetadosisFailureReceipt>> {
        let Some(receipt @ WwdTerminalReceipt::MetadosisFailure(common)) =
            self.read_terminal_receipt(worldwide_day)?
        else {
            return Ok(None);
        };
        let context =
            self.terminal_receipt_validation_context(worldwide_day, Some(expected_value_routed))?;
        receipt.validate(context)?;
        Ok(Some(MetadosisFailureReceipt {
            worldwide_day,
            value_routed: common.value_routed,
            carry_over_before: common.carry_over_before,
            carry_over_after: common.carry_over_after,
            retirement: common.retirement,
            block_number: common.block_number,
        }))
    }

    pub(crate) fn read_missed_offering_receipt(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<MissedOfferingReceipt>> {
        let Some(receipt) = self.read_terminal_receipt(worldwide_day)? else {
            return Ok(None);
        };
        receipt.validate(self.terminal_receipt_validation_context(worldwide_day, None)?)?;
        match receipt {
            WwdTerminalReceipt::MissedOffering(common) => Ok(Some(MissedOfferingReceipt {
                worldwide_day,
                value_routed: common.value_routed,
                carry_over_before: common.carry_over_before,
                carry_over_after: common.carry_over_after,
                retirement: common.retirement,
                block_number: common.block_number,
            })),
            WwdTerminalReceipt::CapacityForfeiture { .. } => Ok(None),
            WwdTerminalReceipt::MetadosisFailure(_) => Err(storage_corruption_message(
                "Metadosis WWD terminal receipt is invalid",
            )),
        }
    }

    pub(crate) fn write_capacity_forfeiture_receipt(
        &mut self,
        receipt: CapacityForfeitureReceipt,
    ) -> Result<()> {
        self.write_terminal_receipt(WwdTerminalReceipt::CapacityForfeiture {
            common: TerminalReceiptCommon {
                worldwide_day: receipt.worldwide_day,
                value_routed: receipt.value_routed,
                carry_over_before: receipt.carry_over_before,
                carry_over_after: receipt.carry_over_after,
                retirement: receipt.retirement,
                block_number: receipt.block_number,
            },
            detail: CapacityForfeitureDetail {
                max_retained_wwds: receipt.max_retained_wwds,
                retained_count_before: receipt.retained_count_before,
                sealed_collection_root: receipt.sealed_collection_root,
                forfeited_count: receipt.forfeited_count,
                forfeited_nominal: receipt.forfeited_nominal,
                source_generation: receipt.source_generation,
                retired_generation: receipt.retired_generation,
            },
        })
    }

    pub(crate) fn read_capacity_forfeiture_receipt(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<CapacityForfeitureReceipt>> {
        let Some(receipt) = self.read_terminal_receipt(worldwide_day)? else {
            return Ok(None);
        };
        receipt.validate(self.terminal_receipt_validation_context(worldwide_day, None)?)?;
        match receipt {
            WwdTerminalReceipt::CapacityForfeiture { common, detail } => {
                Ok(Some(CapacityForfeitureReceipt {
                    worldwide_day,
                    max_retained_wwds: detail.max_retained_wwds,
                    retained_count_before: detail.retained_count_before,
                    value_routed: common.value_routed,
                    carry_over_before: common.carry_over_before,
                    carry_over_after: common.carry_over_after,
                    sealed_collection_root: detail.sealed_collection_root,
                    forfeited_count: detail.forfeited_count,
                    forfeited_nominal: detail.forfeited_nominal,
                    source_generation: detail.source_generation,
                    retired_generation: detail.retired_generation,
                    retirement: common.retirement,
                    block_number: common.block_number,
                }))
            }
            WwdTerminalReceipt::MissedOffering(_) => Ok(None),
            WwdTerminalReceipt::MetadosisFailure(_) => Err(storage_corruption_message(
                "capacity-forfeiture terminal receipt has no detail",
            )),
        }
    }
}
