pub mod aggregate;
pub mod api;
pub mod constants;
pub mod errors;
#[cfg(any(test, feature = "test-utils"))]
pub mod genesis;
pub mod lifecycle;
pub(crate) mod queries;
pub mod schema;

pub use aggregate::{
    close_worldwide_day, create_worldwide_day, NewWwdSchedule, ValidatedWwdAggregate, WwdProjection,
};
pub use api::{is_offering_day, offering_worldwide_days, worldwide_day, worldwide_days};
pub use lifecycle::{
    advance_worldwide_days, advance_worldwide_days_command,
    bootstrap_genesis_worldwide_days_command, compute_wwd_schedule, create_worldwide_day_for_date,
    first_wwd_for_genesis, snapshot_forming_vwap, validate_wwd_timestamp,
};
pub use schema::{WorldwideDay as WorldwideDayRecord, WorldwideDayEntryExt};
