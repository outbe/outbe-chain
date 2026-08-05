/// Forming period: 50 hours (UTC-12 to UTC+14 = 50h span).
pub const FORMING_PERIOD_HOURS: u64 = 50;

/// lookback delay: 502 hours (~21 days).
pub const LOOKBACK_DELAY_HOURS: u64 = 502;

/// offering period: 50 hours.
pub const OFFERING_PERIOD_HOURS: u64 = 50;

/// Waiting period before processing: 12 hours.
pub const WAITING_PERIOD_HOURS: u64 = 12;

/// Fresh-devnet production creates at most one WorldwideDay per UTC day.
pub const WWD_CREATION_CADENCE_HOURS: u64 = 24;

/// Production advances the outer WWD reducer at midnight (the daily
/// Metadosis command) and at noon (the dedicated Cycle trigger).
pub const WWD_ADVANCE_TICK_CADENCE_HOURS: u64 = 12;

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

const NORMAL_PIPELINE_HOURS: u64 =
    FORMING_PERIOD_HOURS + LOOKBACK_DELAY_HOURS + OFFERING_PERIOD_HOURS + WAITING_PERIOD_HOURS;
const BOOTSTRAP_PIPELINE_HOURS: u64 = FORMING_PERIOD_HOURS
    + BOOTSTRAP_LOOKBACK_DELAY_HOURS
    + BOOTSTRAP_OFFERING_PERIOD_HOURS
    + WAITING_PERIOD_HOURS;
const NORMAL_PIPELINE_WWDS: usize =
    NORMAL_PIPELINE_HOURS.div_ceil(WWD_CREATION_CADENCE_HOURS) as usize;
const BOOTSTRAP_PIPELINE_WWDS: usize =
    BOOTSTRAP_PIPELINE_HOURS.div_ceil(WWD_CREATION_CADENCE_HOURS) as usize;

/// Maximum pre-admission population implied by the production cadence and
/// phase durations. The extra entry is the current day created on the first
/// post-halt midnight before the reducer drains the bounded pre-halt pipeline.
///
/// Normal windows dominate: ceil((50 + 502 + 50 + 12) / 24) + 1 = 27.
/// The two 12-hour advancement opportunities per 24-hour creation cadence
/// drain a halt backlog at a net rate of one WWD per day.
pub const MAX_PIPELINE_WWDS: usize = if NORMAL_PIPELINE_WWDS > BOOTSTRAP_PIPELINE_WWDS {
    NORMAL_PIPELINE_WWDS + 1
} else {
    BOOTSTRAP_PIPELINE_WWDS + 1
};

/// Maximum WWD records kept. This is the canonical historical retention bound,
/// not a concurrent OCOMP job admission limit.
pub const MAX_RECORDS_KEPT: usize = 365;

/// READY/OFFCHAIN_PENDING work can use the same population already bounded by
/// the canonical WWD record-retention policy. OCOMP does not impose a smaller
/// concurrent live-job cap.
pub const MAX_RETAINED_WWDS: usize = MAX_RECORDS_KEPT;

/// Exact bound for every scan of the active WorldwideDay aggregate.
pub const MAX_ACTIVE_WWDS: usize = MAX_PIPELINE_WWDS + MAX_RETAINED_WWDS;

/// Under continuing midnight/noon ticks, an already-active candidate can have
/// at most this many older protocol-order admissions ahead of it.
pub const MAX_ADMISSION_WAIT_TICKS: usize = MAX_PIPELINE_WWDS;
pub const MAX_ADMISSION_WAIT_HOURS: u64 =
    MAX_ADMISSION_WAIT_TICKS as u64 * WWD_ADVANCE_TICK_CADENCE_HOURS;

/// UTC+14 offset in seconds (14 * 3600).
pub const UTC_PLUS_14_OFFSET: u64 = 50_400;

/// Seconds per hour.
pub const SECONDS_PER_HOUR: u64 = 3600;
