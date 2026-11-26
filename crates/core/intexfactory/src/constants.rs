//! IntexFactory constants: external contract addresses called via `storage.call`, and
//! the caps and timings of the cross-chain sends. The series terms live in Intex.

pub use outbe_core_common::addresses::INTEX_NFT1155_ADDRESS;

/// Series one ISSUANCE_INSTRUCTIONS message may carry. Mirrors `MAX_SERIES_PER_ISSUANCE`.
pub const MAX_SERIES_PER_MESSAGE: usize = 16;

/// Series one INTEX_SERIES_CALL_NOTICE message may carry. Mirrors the
/// codec's `MAX_SERIES_PER_CALL_NOTICE`. A wider group is sent in several messages.
pub const MAX_SERIES_PER_CALL_NOTICE: usize = 8;
