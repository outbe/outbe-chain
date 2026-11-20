//! Witness schema for Promis operations.
//!
//! Tag 2 is `PromisOpApplied`, including a signed business rejection.

use crate::protocol::PromisOpResult;

outbe_core_common::impl_confidential_witness_schema!(PromisOpResult, PromisOpApplied);
