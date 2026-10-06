use alloy_primitives::{B256, U256};
use outbe_compressed_entities::RetirementOutcome;
use outbe_primitives::{error::Result, time::WorldwideDay};

use super::TerminalReceiptValidationContext;
use crate::{
    constants::MAX_RETAINED_WWDS,
    errors::storage_corruption_message,
    schema::{status, terminal_outcome},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalReceiptCommon {
    pub worldwide_day: WorldwideDay,
    pub value_routed: U256,
    pub carry_over_before: U256,
    pub carry_over_after: U256,
    pub retirement: RetirementOutcome,
    pub block_number: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CapacityForfeitureDetail {
    pub max_retained_wwds: u32,
    pub retained_count_before: u32,
    pub sealed_collection_root: B256,
    pub forfeited_count: u32,
    pub forfeited_nominal: U256,
    pub source_generation: u64,
    pub retired_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WwdTerminalReceipt {
    MissedOffering(TerminalReceiptCommon),
    CapacityForfeiture {
        common: TerminalReceiptCommon,
        detail: CapacityForfeitureDetail,
    },
    MetadosisFailure(TerminalReceiptCommon),
}

impl WwdTerminalReceipt {
    pub(crate) const fn common(&self) -> &TerminalReceiptCommon {
        match self {
            Self::MissedOffering(common)
            | Self::MetadosisFailure(common)
            | Self::CapacityForfeiture { common, .. } => common,
        }
    }

    #[cfg(test)]
    pub(crate) fn common_mut(&mut self) -> &mut TerminalReceiptCommon {
        match self {
            Self::MissedOffering(common)
            | Self::MetadosisFailure(common)
            | Self::CapacityForfeiture { common, .. } => common,
        }
    }

    pub(crate) const fn outcome(&self) -> u8 {
        match self {
            Self::MissedOffering(_) => terminal_outcome::MISSED_OFFERING,
            Self::CapacityForfeiture { .. } => terminal_outcome::CAPACITY_FORFEITURE,
            Self::MetadosisFailure(_) => terminal_outcome::METADOSIS_FAILURE,
        }
    }

    pub(crate) fn validate(&self, context: TerminalReceiptValidationContext) -> Result<()> {
        self.common().validate(context)?;
        if let Self::CapacityForfeiture { common, detail } = self {
            detail.validate(common.retirement)?;
        }
        Ok(())
    }
}

impl TerminalReceiptCommon {
    fn validate(&self, context: TerminalReceiptValidationContext) -> Result<()> {
        let terminal_membership = context.status == status::FAILED
            && context.active_memberships == 0
            && context.closed_memberships == 1;
        let conserved = self.value_routed == context.expected_value_routed
            && self.carry_over_before.checked_add(self.value_routed) == Some(self.carry_over_after);
        if !terminal_membership || !conserved || self.block_number == 0 {
            return Err(storage_corruption_message(
                "Metadosis WWD terminal receipt is invalid",
            ));
        }
        Ok(())
    }
}

impl CapacityForfeitureDetail {
    fn validate(&self, retirement: RetirementOutcome) -> Result<()> {
        let max = u32::try_from(MAX_RETAINED_WWDS)
            .map_err(|_| storage_corruption_message("retained cap exceeds u32"))?;
        let capacity_policy = self.max_retained_wwds == max && self.retained_count_before == max;
        let generation_transition = self.source_generation == 0 && self.retired_generation == 1;
        if !capacity_policy || !generation_transition || !self.valid_retirement(retirement) {
            return Err(storage_corruption_message(
                "capacity-forfeiture detail differs from terminal receipt",
            ));
        }
        Ok(())
    }

    fn valid_retirement(&self, retirement: RetirementOutcome) -> bool {
        match retirement {
            RetirementOutcome::NotPresent => {
                self.sealed_collection_root.is_zero()
                    && self.forfeited_count == 0
                    && self.forfeited_nominal.is_zero()
            }
            RetirementOutcome::Requested => !self.sealed_collection_root.is_zero(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> WwdTerminalReceipt {
        WwdTerminalReceipt::CapacityForfeiture {
            common: TerminalReceiptCommon {
                worldwide_day: WorldwideDay::new(42),
                value_routed: U256::from(5),
                carry_over_before: U256::from(2),
                carry_over_after: U256::from(7),
                retirement: RetirementOutcome::NotPresent,
                block_number: 9,
            },
            detail: CapacityForfeitureDetail {
                max_retained_wwds: MAX_RETAINED_WWDS as u32,
                retained_count_before: MAX_RETAINED_WWDS as u32,
                sealed_collection_root: B256::ZERO,
                forfeited_count: 0,
                forfeited_nominal: U256::ZERO,
                source_generation: 0,
                retired_generation: 1,
            },
        }
    }

    fn context() -> TerminalReceiptValidationContext {
        TerminalReceiptValidationContext::new(status::FAILED, 0, 1, U256::from(5))
    }

    #[test]
    fn common_requires_terminal_membership_value_conservation_and_block() {
        assert!(receipt().validate(context()).is_ok());
        for invalid in [
            TerminalReceiptValidationContext::new(status::READY, 0, 1, U256::from(5)),
            TerminalReceiptValidationContext::new(status::FAILED, 1, 1, U256::from(5)),
            TerminalReceiptValidationContext::new(status::FAILED, 0, 0, U256::from(5)),
            TerminalReceiptValidationContext::new(status::FAILED, 0, 2, U256::from(5)),
            TerminalReceiptValidationContext::new(status::FAILED, 0, 1, U256::from(6)),
        ] {
            assert!(receipt().validate(invalid).is_err());
        }
        for field in 0..3 {
            let mut corrupt = receipt();
            let common = corrupt.common_mut();
            match field {
                0 => common.block_number = 0,
                1 => common.carry_over_after += U256::from(1),
                _ => common.carry_over_before = U256::MAX,
            }
            assert!(corrupt.validate(context()).is_err());
        }
    }

    #[test]
    fn capacity_checks_exact_cap_generations_and_retirement_evidence() {
        for field in 0..7 {
            let mut corrupt = receipt();
            let WwdTerminalReceipt::CapacityForfeiture { detail, .. } = &mut corrupt else {
                unreachable!()
            };
            match field {
                0 => detail.max_retained_wwds += 1,
                1 => detail.retained_count_before += 1,
                2 => detail.source_generation = 1,
                3 => detail.retired_generation = 2,
                4 => detail.sealed_collection_root = B256::repeat_byte(1),
                5 => detail.forfeited_count = 1,
                _ => detail.forfeited_nominal = U256::from(1),
            }
            assert!(corrupt.validate(context()).is_err());
        }
        let mut requested = receipt();
        requested.common_mut().retirement = RetirementOutcome::Requested;
        assert!(requested.validate(context()).is_err());
        let WwdTerminalReceipt::CapacityForfeiture { detail, .. } = &mut requested else {
            unreachable!()
        };
        detail.sealed_collection_root = B256::repeat_byte(1);
        detail.forfeited_count = u32::MAX;
        detail.forfeited_nominal = U256::MAX;
        assert!(requested.validate(context()).is_ok());
    }
}
