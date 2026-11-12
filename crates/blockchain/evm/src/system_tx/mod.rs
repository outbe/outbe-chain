//! System-transaction execution schedule, assembly and layout validation.
pub use outbe_primitives::system_tx::*;
mod builder;
mod fsm;
mod layout;
mod order;
pub use builder::assemble_prefix_system_txs;
pub use fsm::{BlockExecutionFsm, BlockSystemPlan, PlannedSystemStep};
pub use layout::{validate_system_layout, validate_system_tx_set_for_publication};
pub use order::{begin_step, consensus_step, end_step, BlockOrder, ProtocolStep};
pub mod binding {
    pub use super::layout::validate_system_layout;
    pub use outbe_primitives::system_tx::binding::*;
}
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
#[cfg(any(test, feature = "test-utils"))]
pub use test_utils::TEST_BLOCK_ORDER;
#[cfg(test)]
mod tests;
