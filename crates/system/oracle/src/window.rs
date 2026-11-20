//! The window of the 24-hour VWAP and the snapshot id that names one window.

use alloy_primitives::U256;
use outbe_chain_constants::VWAP_24H_WINDOW_SECONDS;
use outbe_primitives::error::Result;

use crate::constants::VWAP_HOUR_SECONDS;
use crate::errors::OracleError;

/// One window of the 24-hour VWAP: `[cutoff - 24 h, cutoff)`. The id is the
/// cutoff, a whole UTC hour in Unix seconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VwapSnapshotId {
    cutoff: u64,
}

impl VwapSnapshotId {
    fn new(cutoff: u64) -> Result<Self> {
        if !cutoff.is_multiple_of(VWAP_HOUR_SECONDS) || cutoff < VWAP_24H_WINDOW_SECONDS {
            return Err(OracleError::InvalidVwapSnapshot.into());
        }
        Ok(Self { cutoff })
    }

    pub fn from_u256(word: U256) -> Result<Self> {
        let cutoff = u64::try_from(word).map_err(|_| OracleError::InvalidVwapSnapshot)?;
        Self::new(cutoff)
    }

    pub fn to_u256(self) -> U256 {
        U256::from(self.cutoff)
    }

    pub fn cutoff(self) -> u64 {
        self.cutoff
    }

    pub fn start(self) -> u64 {
        self.cutoff - VWAP_24H_WINDOW_SECONDS
    }
}

/// The snapshot of `block_timestamp`: its window ends at the last whole UTC
/// hour at or before that time.
pub fn vwap_24h_snapshot_id(block_timestamp: u64) -> Result<VwapSnapshotId> {
    VwapSnapshotId::new(block_timestamp - block_timestamp % VWAP_HOUR_SECONDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 1_753_228_800;
    const HOUR: u64 = 3_600;

    fn window(timestamp: u64) -> (u64, u64) {
        let snapshot = vwap_24h_snapshot_id(timestamp).unwrap();
        (snapshot.start(), snapshot.cutoff())
    }

    #[test]
    fn vwap_24h_window_is_a_fixed_24_hours() {
        let at = |h: u64, m: u64, s: u64| DAY + h * HOUR + m * 60 + s;
        assert_eq!(VWAP_24H_WINDOW_SECONDS, 24 * HOUR);
        assert_eq!(
            window(at(14, 30, 0)),
            (at(14, 0, 0) - DAY_SECONDS, at(14, 0, 0))
        );
        assert_eq!(
            window(at(14, 59, 59)),
            (at(14, 0, 0) - DAY_SECONDS, at(14, 0, 0))
        );
        assert_eq!(
            window(at(15, 10, 0)),
            (at(15, 0, 0) - DAY_SECONDS, at(15, 0, 0))
        );
        assert_eq!(window(at(0, 20, 0)), (DAY - DAY_SECONDS, DAY));
    }

    const DAY_SECONDS: u64 = 24 * HOUR;

    #[test]
    fn a_window_before_the_epoch_is_rejected() {
        assert!(vwap_24h_snapshot_id(23 * HOUR).is_err());
        assert!(vwap_24h_snapshot_id(24 * HOUR).is_ok());
    }

    #[test]
    fn a_snapshot_id_is_the_window_end() {
        let snapshot = vwap_24h_snapshot_id(DAY + 10 * HOUR + 5).unwrap();
        let word = snapshot.to_u256();
        assert_eq!(word, U256::from(DAY + 10 * HOUR));
        assert_eq!(VwapSnapshotId::from_u256(word).unwrap(), snapshot);
    }

    #[test]
    fn malformed_snapshot_words_are_rejected() {
        let word = vwap_24h_snapshot_id(DAY + 10 * HOUR).unwrap().to_u256();
        for bad in [
            word + U256::ONE,
            word | (U256::ONE << 64),
            U256::from(23 * HOUR),
            U256::ZERO,
        ] {
            assert!(VwapSnapshotId::from_u256(bad).is_err(), "{bad:#x}");
        }
    }
}
