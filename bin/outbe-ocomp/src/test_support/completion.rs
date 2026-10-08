use alloy_primitives::U256;
use outbe_ocomp_protocol::{
    intent::DayType,
    result::{CompletionStatus, MetadosisCompletionSummaryV1},
};

pub fn zero_completion_summary(
    wwd: u32,
    pending_nonce: u64,
    logical_height: u64,
    logical_time: u64,
) -> MetadosisCompletionSummaryV1 {
    MetadosisCompletionSummaryV1 {
        wwd,
        pending_nonce,
        day_type: DayType::Green,
        tribute_nominal_total: U256::ZERO,
        day_limit: U256::ZERO,
        gratis_demand: U256::ZERO,
        day_gratis_limit_minor: U256::ZERO,
        lysis_limit_minor: U256::ZERO,
        desis_limit_minor: U256::ZERO,
        lysis_allocation_minor: U256::ZERO,
        unused_lysis_limit_minor: U256::ZERO,
        carry_over_credit: U256::ZERO,
        status: CompletionStatus::Completed,
        logical_evaluation_height: logical_height,
        logical_evaluation_time: logical_time,
    }
}
