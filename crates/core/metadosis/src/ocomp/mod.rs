//! Consensus job lifecycle owned by Metadosis.
//!
//! The PoC exposes one closed Lysis V1 request/expiry lifecycle. Local worker
//! progress is deliberately absent from these types.

pub mod activation;
pub mod expiry;
pub mod fork;
pub mod request;
pub mod schema;
pub mod state;
#[cfg(feature = "test-utils")]
#[doc(hidden)]
pub mod test_support;
pub mod views;
pub mod vote;
