//! IntexFactory constants: external contract addresses called via `storage.call`, and
//! the caps and timings of the cross-chain sends. The series terms live in Intex.

use alloy_primitives::U256;

pub use outbe_primitives::addresses::{INTEX_NFT1155_ADDRESS, ORIGIN_ROUTER_ADDRESS};

/// Router calls a Called notice gets before it is dropped.
pub const MAX_CALLED_NOTICE_ATTEMPTS: u8 = 8;

/// Pause before a refused notice is sent again, so the 8 sends span about 35 minutes
/// of a router outage.
#[cfg(not(feature = "e2e-test"))]
pub const NOTICE_RETRY_SECONDS: u64 = 300;
#[cfg(feature = "e2e-test")]
pub const NOTICE_RETRY_SECONDS: u64 = 30;

/// Consecutive wholly refused runs that end a block's send: a router that refuses every
/// send would otherwise spend the whole budget on calls that cannot land.
pub const MAX_REFUSED_RUNS_PER_BLOCK: u32 = 3;

/// Router calls one `intex_drain_parked` firing may make; each is a view read plus a cross-chain send.
pub const MAX_PARKED_CALLS_PER_FIRING: u32 = 16;

/// Consecutive failures that end a parked pass: an empty relay float fails every entry alike.
pub const MAX_PARKED_FAILURES_PER_FIRING: u32 = 3;

/// Series one ISSUANCE_INSTRUCTIONS message may carry. Mirrors `MAX_SERIES_PER_ISSUANCE`.
pub const MAX_SERIES_PER_MESSAGE: usize = 23;

/// Series one MARK_CALLED message may carry. Mirrors the
/// codec's `MAX_SERIES_PER_MARK`. A wider group is sent in several messages.
pub const MAX_SERIES_PER_MARK: usize = 8;

/// Bit that marks a Settled NFT token id. A series id is 14 bytes, so the issued id space ends at
/// 2**112 and this bit sits directly above it. The classes cannot collide, and clearing the bit
/// recovers the series. Mirrors `IntexNFT1155._SETTLED_TAG`.
/// `settled_token_id_tags_the_series_id` reads that Solidity constant.
pub const SETTLED_TAG: U256 = U256::from_limbs([0, 1 << 48, 0, 0]);
