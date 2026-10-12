//! Module-local protocol constants.

/// How long a CCA stables reservation stays exercisable. After this window the
/// reservation can no longer be released to its owner. It can only be
/// returned to its origin vault.
pub const RESERVATION_TTL_SECS: u64 = 15 * 60;
