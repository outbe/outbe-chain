/// Floor-price markup rate: floor = `entry x (100 + FLOOR_RATE) / 100`
/// (8 => 1.08x entry).
pub const FLOOR_RATE: u64 = 8;

/// Call-price markup rate: call price = `entry x (100 + CALL_RATE) / 100`
/// (128 => 2.28x entry). Its breach arms a Call Event.
pub const CALL_RATE: u64 = 128;

/// SRA cost rate (share of the full agent cost): cost = `full x SRA_RATE / 100`
/// (64 => 0.64x).
pub const SRA_RATE: u64 = 64;

/// Positions the daily sweep may retire before it gives out.
pub const MAX_POSITION_EXPIRIES_PER_RUN: u32 = 256;
