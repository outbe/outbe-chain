#[cfg(all(feature = "test-enclave", feature = "production-enclave"))]
compile_error!(
    "outbe-tribute: `test-enclave` selects an in-process stand-in that skips \
     attestation, and it has been unified into a production build. Some \
     dependency edge enables it outside [dev-dependencies] — cargo features \
     are additive, so one such edge anywhere in the graph is enough."
);

pub mod codec;
pub mod day_schema;
mod day_state;
pub mod enclave_client;
pub mod errors;
mod issuance;
pub mod precompile;
pub mod record;
pub mod runtime;
pub mod schema;
pub mod state;
pub mod state_api;
pub mod state_proof;
pub mod storage_records;

pub use record::{TributeMetadata, TributeRecord};
pub use runtime::{LoadedTribute, LysisOutcome, PricedCurrencies};
pub use schema::{DayTotals, TributeContract, TributeData};

#[cfg(test)]
mod tests;

pub use codec::{canonical_body, from_canonical_body};
