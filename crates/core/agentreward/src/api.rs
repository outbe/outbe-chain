//! Daily allocation and bounded settlement. Amounts are protocol units, not native COEN.
pub use crate::distribution::{
    abandon_day_agent_rewards, allocate_day_agent_rewards, process_agent_rewards,
    DayAgentAllocation,
};
