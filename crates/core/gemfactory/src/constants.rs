/// Floor-price markup rate: floor = `entry × (100 + FLOOR_RATE) / 100`
/// (8 => 1.08x entry).
pub const FLOOR_RATE: u64 = 8;

/// Call-price markup rate: call price = `entry × (100 + CALL_RATE) / 100`
/// (128 => 2.28x entry). Its breach arms a Call Event.
pub const CALL_RATE: u64 = 128;

/// SRA cost rate (share of the full agent cost): cost = `full × SRA_RATE / 100`
/// (64 => 0.64x).
pub const SRA_RATE: u64 = 64;

/// GemPosition validity period: a parked Intex expires this long after
/// `parked_at`; no new gems may be issued afterward. 1 year.
pub const POSITION_VALIDITY_SECONDS: u64 = 365 * 24 * 3600;

/// Decimals a settlement stablecoin must carry. Gem amounts are six-decimal
/// monetary values and the transfer moves raw token units, so an asset on any
/// other scale would move the wrong sum.
pub const SETTLEMENT_ASSET_DECIMALS: u8 = 6;

/// Max positions the expiry sweep visits per daily run; the cursor resumes the
/// rest on the next run so one sweep can never outgrow a block. An entry
/// displaced past the cursor is picked up a day later, which cannot change an
/// outcome: a position stays expired once `POSITION_VALIDITY_SECONDS` lapses.
pub const MAX_POSITION_EXPIRY_VISITS: u32 = 4096;
