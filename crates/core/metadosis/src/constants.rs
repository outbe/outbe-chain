use outbe_chain_constants::{
    DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS, DEFAULT_METADOSIS_LOOKBACK_DELAY_SECONDS,
    DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS,
};

/// Compatibility aliases for capacity proofs and canonical-default tests.
pub const FORMING_PERIOD_HOURS: u64 = DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS / SECONDS_PER_HOUR;

/// lookback delay: 502 hours (~21 days).
pub const LOOKBACK_DELAY_HOURS: u64 = DEFAULT_METADOSIS_LOOKBACK_DELAY_SECONDS / SECONDS_PER_HOUR;

/// offering period: 50 hours.
pub const OFFERING_PERIOD_HOURS: u64 = DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS / SECONDS_PER_HOUR;

/// Default delay from `offering_end` (12:00 UTC) to `metadosis_start` (00:00:01 UTC): 12 hours.
pub const METADOSIS_DELAY_AFTER_OFFERING_HOURS: u64 = 12;

/// Fresh-devnet production creates at most one WorldwideDay per UTC day.
pub const WWD_CREATION_CADENCE_HOURS: u64 = 24;

/// Production ProtocolCycle advances the outer WWD reducer on every aligned
/// UTC-hour boundary.
pub const WWD_ADVANCE_TICK_CADENCE_HOURS: u64 = 1;

/// Symbolic rate: 32% of tribute nominal -> gratis demand.
pub const SYMBOLIC_RATE: u64 = 32;

const NORMAL_PIPELINE_HOURS: u64 = FORMING_PERIOD_HOURS
    + LOOKBACK_DELAY_HOURS
    + OFFERING_PERIOD_HOURS
    + METADOSIS_DELAY_AFTER_OFFERING_HOURS;
const NORMAL_PIPELINE_WWDS: usize =
    NORMAL_PIPELINE_HOURS.div_ceil(WWD_CREATION_CADENCE_HOURS) as usize;

/// Maximum pre-admission population implied by the production cadence and
/// phase durations: `ceil((50 + 430 + 50 + 12) / 24) + 1 = 24`.
pub const MAX_PIPELINE_WWDS: usize = NORMAL_PIPELINE_WWDS + 1;

/// Maximum WWD records kept in the canonical historical retention queue.
pub const MAX_RECORDS_KEPT: usize = 365;

/// READY work uses the population bounded by the canonical WWD retention policy.
pub const MAX_RETAINED_WWDS: usize = MAX_RECORDS_KEPT;

/// Exact bound for every scan of the active WorldwideDay aggregate.
pub const MAX_ACTIVE_WWDS: usize = MAX_PIPELINE_WWDS + MAX_RETAINED_WWDS;

/// Under continuing hourly ticks, an already-active candidate can have
/// at most this many older protocol-order admissions ahead of it.
pub const MAX_ADMISSION_WAIT_TICKS: usize = MAX_PIPELINE_WWDS;
pub const MAX_ADMISSION_WAIT_HOURS: u64 =
    MAX_ADMISSION_WAIT_TICKS as u64 * WWD_ADVANCE_TICK_CADENCE_HOURS;

/// Seconds per hour.
pub const SECONDS_PER_HOUR: u64 = 3600;
