pub use outbe_chain_constants::MAX_PIPELINE_WWDS;
use outbe_chain_constants::{
    DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS, DEFAULT_METADOSIS_LOOKBACK_DELAY_SECONDS,
    DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS,
};

/// Compatibility aliases for capacity proofs and canonical-default tests.
pub const FORMING_PERIOD_HOURS: u64 = DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS / SECONDS_PER_HOUR;

/// lookback delay: 22 hours.
pub const LOOKBACK_DELAY_HOURS: u64 = DEFAULT_METADOSIS_LOOKBACK_DELAY_SECONDS / SECONDS_PER_HOUR;

/// offering period: 50 hours.
pub const OFFERING_PERIOD_HOURS: u64 = DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS / SECONDS_PER_HOUR;

/// Default delay from `offering_end` (12:00 UTC) to `metadosis_start` (00:00:01 UTC): 12 hours.
pub const METADOSIS_DELAY_AFTER_OFFERING_HOURS: u64 = 12;

/// Fresh-devnet production creates at most one WorldwideDay per UTC day.
pub const WWD_CREATION_CADENCE_HOURS: u64 = 24;

/// Maximum WWD records kept in the canonical historical retention queue.
pub const MAX_RECORDS_KEPT: usize = 365;

/// READY work uses the population bounded by the canonical WWD retention policy.
pub const MAX_RETAINED_WWDS: usize = MAX_RECORDS_KEPT;

/// Exact bound for every scan of the active WorldwideDay aggregate.
pub const MAX_ACTIVE_WWDS: usize = MAX_PIPELINE_WWDS + MAX_RETAINED_WWDS;

/// Seconds per hour.
pub const SECONDS_PER_HOUR: u64 = 3600;

/// `IMetadosis.MetadosisCanceled.reason`: the emission day has no Metadosis limit.
pub const METADOSIS_CANCELED_NO_LIMIT: u8 = 1;

/// `IMetadosis.MetadosisFailed.reason`: the Metadosis step reverted.
pub const METADOSIS_FAILED_STEP_REVERTED: u8 = 1;
