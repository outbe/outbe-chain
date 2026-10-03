pub(crate) mod codec;
pub(crate) mod model;
mod store;

use crate::schema::terminal_retirement;
use alloy_primitives::{B256, U256};
use outbe_compressed_entities::RetirementOutcome;
use outbe_primitives::{error::Result, time::WorldwideDay};
mod failure;
pub(crate) use failure::{
    fail_expired_ocomp_day, fail_worldwide_day, ExpiredFailure, FailureSettlement,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MetadosisFailureReceipt {
    pub worldwide_day: WorldwideDay,
    pub value_routed: U256,
    pub carry_over_before: U256,
    pub carry_over_after: U256,
    pub retirement: RetirementOutcome,
    pub block_number: u64,
}

pub(crate) const fn encode_retirement(outcome: RetirementOutcome) -> u8 {
    match outcome {
        RetirementOutcome::NotPresent => terminal_retirement::NOT_PRESENT,
        RetirementOutcome::Requested => terminal_retirement::REQUESTED,
    }
}

fn decode_retirement(value: u8) -> Result<RetirementOutcome> {
    match value {
        terminal_retirement::NOT_PRESENT => Ok(RetirementOutcome::NotPresent),
        terminal_retirement::REQUESTED => Ok(RetirementOutcome::Requested),
        _ => Err(crate::errors::storage_corruption(
            "Metadosis WWD terminal receipt has invalid retirement outcome".into(),
        )),
    }
}
