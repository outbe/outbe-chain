//! Execution-backed snapshot fixtures and CL/EL integration tests.

#[cfg(feature = "snapshot-integration")]
#[doc(hidden)]
pub mod snapshot_native;
#[cfg(test)]
mod tests;
