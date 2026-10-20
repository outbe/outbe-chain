use std::ops::Deref;

use outbe_primitives::{error::Result, storage::StorageHandle};

pub use outbe_tribute::worldwideday::aggregate::{
    WwdDayType, WwdMembership, WwdProjection, WwdStatus,
};

use crate::{
    errors::MetadosisStorageCorruptionError,
    schema::MetadosisContract,
    terminal::{model::WwdTerminalReceipt, TerminalReceiptValidationContext},
};

/// Validated view over the Tribute-owned WWD aggregate paired with
/// Metadosis-owned terminal receipt invariants.
#[derive(Debug)]
pub(crate) struct ValidatedWwdAggregate(outbe_tribute::worldwideday::ValidatedWwdAggregate);

impl Deref for ValidatedWwdAggregate {
    type Target = outbe_tribute::worldwideday::ValidatedWwdAggregate;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ValidatedWwdAggregate {
    pub(crate) fn load_and_validate(storage: StorageHandle<'_>) -> Result<Self> {
        let inner =
            outbe_tribute::worldwideday::ValidatedWwdAggregate::load_and_validate(storage.clone())?;
        let contract = MetadosisContract::new(storage);
        for projection in inner.records() {
            validate_terminal_state(&contract, projection)?;
        }
        Ok(Self(inner))
    }
}

fn validate_terminal_state(
    contract: &MetadosisContract<'_>,
    projection: &WwdProjection,
) -> Result<()> {
    let wwd = projection.worldwide_day;
    let Some(receipt) = contract.read_terminal_receipt(wwd)? else {
        return Ok(());
    };
    let expected_value_routed = if matches!(receipt, WwdTerminalReceipt::MetadosisFailure(_)) {
        contract.failure_value_routed(wwd, projection.metadosis_limit_minor)?
    } else {
        projection.metadosis_limit_minor
    };
    receipt
        .validate(TerminalReceiptValidationContext::new(
            projection.status.as_u8(),
            usize::from(projection.membership == WwdMembership::Active),
            usize::from(projection.membership == WwdMembership::Closed),
            expected_value_routed,
        ))
        .map_err(|_| MetadosisStorageCorruptionError::WwdInvalidTerminalReceipt { wwd }.into())
}
