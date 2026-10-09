//! Storage-level reads and writes for Oracle state.
//!
//! CRUD over the pair registry, exchange rates, votes, penalty counters, the
//! price-snapshot ring buffer, and the stored VWAP snapshots. Computation and
//! orchestration live in `runtime`.

mod feeders;
mod hour_blocks;
mod pairs;
mod penalties;
mod policy_rates;
mod price_entries;
mod rates;
mod snapshot_history;
mod snapshots;
mod utc_day_vwap;
mod votes;
mod wwd_vwap;

pub(crate) use snapshots::hourly_vwap_cell;
