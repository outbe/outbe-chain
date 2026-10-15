//! The terms an Intex series is issued under.

use outbe_primitives::units::SCALE_1E18_U128;

/// Floor price = entry * 1.08.
pub const FLOOR_RATE_PCT: u16 = 8;
/// Call price = entry * 2.28; its breach arms a Call Event.
pub const CALL_RATE_PCT: u16 = 128;

/// Notice an owner gets to settle after a series is Called.
pub const CALL_NOTICE_PERIOD: u32 = 7 * 24 * 3600;

/// Call-trigger evaluation window: the most recent stretch scanned for breaches.
pub const CALL_WINDOW: u32 = 28 * 24 * 3600;
/// Call-trigger threshold: how much of the window must be in breach to force-call.
pub const CALL_THRESHOLD: u32 = 21 * 24 * 3600;

/// Commit-entry bond on the target-chain auction: 100M WCOEN in native
/// 18-decimal payment-token units.
pub const COMMIT_BOND_MINOR: u128 = 100_000_000 * SCALE_1E18_U128;
