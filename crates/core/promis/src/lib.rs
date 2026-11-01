#[cfg(all(feature = "test-enclave", feature = "production-enclave"))]
compile_error!(
    "outbe-promis: `test-enclave` selects an in-process stand-in that skips \
     attestation, and it has been unified into a production build. Some \
     dependency edge enables it outside [dev-dependencies] — cargo features \
     are additive, so one such edge anywhere in the graph is enough."
);

pub mod api;
pub mod enclave_client;
pub mod errors;
pub mod precompile;
mod runtime;
mod schema;
mod state;

pub use schema::Promis;

#[cfg(test)]
mod tests;
