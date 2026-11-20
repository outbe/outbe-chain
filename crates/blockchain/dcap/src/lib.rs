//! DCAP quote verification, canonical evidence, and pinned native QVL integration.
pub mod byte_cursor;
pub mod dcap_protocol;
#[cfg(feature = "collateral")]
pub mod dcap_v1;
pub mod errors;
pub mod host_collateral;
#[cfg(feature = "native-dcap")]
pub mod native_qvl;
#[cfg(all(feature = "collateral", not(feature = "native-dcap")))]
mod native_qvl;
pub mod quote;
pub mod release_dcap_artifacts;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
pub mod verifier;
