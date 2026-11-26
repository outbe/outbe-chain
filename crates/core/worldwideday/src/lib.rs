//! The Worldwide Day: its schedule, its pipeline, its seal and its reads.

pub mod aggregate;
pub mod api;
pub mod constants;
pub mod errors;
pub mod lease;
pub mod lifecycle;
pub mod precompile;
mod queries;
pub mod record;
pub mod schema;

pub use aggregate::{
    close_worldwide_day, open_worldwide_day, NewWwdSchedule, ValidatedWwdAggregate,
};
pub use api::{
    emission_day_of, get_worldwide_day, is_offering_day, is_worldwide_day_sealed,
    offering_worldwide_days, record_worldwide_day_nod, seal_worldwide_day, worldwide_day_totals,
    worldwide_days, WorldwideDayTotals,
};
pub use lifecycle::{
    advance_worldwide_days, advance_worldwide_days_command,
    bootstrap_genesis_worldwide_days_command, compute_wwd_schedule, first_wwd_for_genesis,
    open_worldwide_day_for_date, validate_wwd_timestamp,
};
pub use record::{WorldwideDay as WorldwideDayRecord, WorldwideDayEntryExt};
pub use schema::WorldwideDayContract;

#[cfg(test)]
mod tests;
