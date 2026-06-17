/// Forming period: 50 hours (UTC-12 to UTC+14 = 50h span).
pub const FORMING_PERIOD_HOURS: u64 = 50;

/// Default lookback delay: 502 hours (~21 days).
pub const DEFAULT_LOOKBACK_DELAY_HOURS: u64 = 502;

/// Default offering period: 50 hours.
pub const DEFAULT_OFFERING_PERIOD_HOURS: u64 = 50;

/// Waiting period before processing: 12 hours.
pub const WAITING_PERIOD_HOURS: u64 = 12;

/// Symbolic rate: 32% of tribute nominal → gratis demand.
pub const SYMBOLIC_RATE: u64 = 32;

/// RED day reduction coefficient: divide by 8.
pub const RED_DAY_REDUCTION_COEF: u64 = 8;

/// Bootstrap duration (hours) for dev/testnet.
pub const BOOTSTRAP_DURATION_HOURS: u64 = 504;

/// Bootstrap lookback delay: 0 hours.
pub const BOOTSTRAP_LOOKBACK_DELAY_HOURS: u64 = 0;

/// Bootstrap offering period: 48 hours.
pub const BOOTSTRAP_OFFERING_PERIOD_HOURS: u64 = 48;

/// Maximum day metadosis limit records kept.
pub const MAX_DAY_LIMITS_KEPT: usize = 30;

/// Completed worldwide day retention (days).
pub const COMPLETED_WWD_RETENTION_DAYS: u64 = 30;

/// UTC+14 offset in seconds (14 * 3600).
pub const UTC_PLUS_14_OFFSET: u64 = 50_400;

/// Seconds per hour.
pub const SECONDS_PER_HOUR: u64 = 3600;
