//! Witness schema for a recorded Gratis operation.
//!
//! Tag 1 is `GratisOpApplied`, including a signed business rejection.
//! NOD mine uses tags 9 and 8 on the types in [`crate::nod_mine`].

use crate::protocol::GratisOpResult;

outbe_core_common::impl_confidential_witness_schema!(GratisOpResult, GratisOpApplied);
