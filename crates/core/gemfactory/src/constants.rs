/// Floor-price markup rate: floor = `entry x (100 + FLOOR_RATE) / 100`
/// (8 => 1.08x entry).
pub const FLOOR_RATE: u64 = 8;

/// Call-price markup rate: call price = `entry x (100 + CALL_RATE) / 100`
/// (128 => 2.28x entry). Its breach arms a Call Event.
pub const CALL_RATE: u64 = 128;

/// SRA cost rate (share of the full agent cost): cost = `full x SRA_RATE / 100`
/// (64 => 0.64x).
pub const SRA_RATE: u64 = 64;

/// GemPosition validity period: a parked Intex expires this long after
/// `parked_at`; no new gems may be issued afterward. 1 year.
pub const POSITION_VALIDITY_SECONDS: u64 = 365 * 24 * 3600;

/// Positions the daily sweep may retire before it gives out.
pub const MAX_POSITION_EXPIRIES_PER_RUN: u32 = 256;

/// Failed expiry attempts in a row before the stall is announced on chain.
pub const EXPIRY_STALL_THRESHOLD: u32 = 3;
