//! Trailing VWAP windows: the trusted pricing policy and the snapshot identity it
//! selects for a block timestamp.

use alloy_primitives::U256;
use outbe_chain_constants::{
    DEFAULT_VWAP_LOOKBACK_SECONDS, DEFAULT_VWAP_POLICY_VERSION,
    DEFAULT_VWAP_UPDATE_INTERVAL_SECONDS,
};
use outbe_primitives::error::Result;
use outbe_primitives::time::SECONDS_PER_DAY;

use crate::constants::MAX_SNAPSHOT_RETENTION_SECONDS;
use crate::errors::OracleError;

const CUTOFF_BITS: usize = 64;
const INTERVAL_SHIFT: usize = CUTOFF_BITS;
const LOOKBACK_SHIFT: usize = INTERVAL_SHIFT + 32;
const VERSION_SHIFT: usize = LOOKBACK_SHIFT + 32;
const PACKED_BITS: usize = VERSION_SHIFT + 32;

/// Snapshots cover `[cutoff - lookback_seconds, cutoff)`; cutoffs fall every
/// `update_interval_seconds` from the UTC epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VwapPolicy {
    pub policy_version: u32,
    pub vwap_lookback_seconds: u64,
    pub vwap_update_interval_seconds: u64,
}

/// Production policy: an eight-hour lookback refreshed at every whole UTC hour.
pub const DEFAULT_VWAP_POLICY: VwapPolicy = VwapPolicy {
    policy_version: DEFAULT_VWAP_POLICY_VERSION,
    vwap_lookback_seconds: DEFAULT_VWAP_LOOKBACK_SECONDS,
    vwap_update_interval_seconds: DEFAULT_VWAP_UPDATE_INTERVAL_SECONDS,
};

/// The policy the protocol constants pin for this network.
pub fn active_vwap_policy() -> VwapPolicy {
    VwapPolicy {
        policy_version: outbe_chain_constants::get_vwap_policy_version(),
        vwap_lookback_seconds: outbe_chain_constants::get_vwap_lookback_seconds(),
        vwap_update_interval_seconds: outbe_chain_constants::get_vwap_update_interval_seconds(),
    }
}

impl VwapPolicy {
    pub fn validate(&self) -> Result<()> {
        let interval = self.vwap_update_interval_seconds;
        let lookback = self.vwap_lookback_seconds;
        let supported = interval > 0
            && lookback > 0
            && SECONDS_PER_DAY.is_multiple_of(interval)
            && lookback.is_multiple_of(interval)
            && lookback <= MAX_SNAPSHOT_RETENTION_SECONDS;
        if supported {
            Ok(())
        } else {
            Err(OracleError::InvalidVwapPolicy.into())
        }
    }
}

/// Policy version, lookback, update interval and cutoff of one snapshot, packed
/// into a single word.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VwapSnapshotId {
    policy: VwapPolicy,
    cutoff: u64,
}

impl VwapSnapshotId {
    fn new(policy: VwapPolicy, cutoff: u64) -> Result<Self> {
        policy.validate()?;
        if !cutoff.is_multiple_of(policy.vwap_update_interval_seconds)
            || cutoff < policy.vwap_lookback_seconds
        {
            return Err(OracleError::InvalidVwapSnapshot.into());
        }
        Ok(Self { policy, cutoff })
    }

    pub fn from_u256(word: U256) -> Result<Self> {
        if word.bit_len() > PACKED_BITS {
            return Err(OracleError::InvalidVwapSnapshot.into());
        }
        let field = |shift: usize, bits: usize| -> u64 {
            ((word >> shift) & ((U256::ONE << bits) - U256::ONE)).to::<u64>()
        };
        let policy = VwapPolicy {
            policy_version: field(VERSION_SHIFT, 32) as u32,
            vwap_lookback_seconds: field(LOOKBACK_SHIFT, 32),
            vwap_update_interval_seconds: field(INTERVAL_SHIFT, 32),
        };
        Self::new(policy, field(0, CUTOFF_BITS))
            .map_err(|_| OracleError::InvalidVwapSnapshot.into())
    }

    pub fn to_u256(self) -> U256 {
        (U256::from(self.policy.policy_version) << VERSION_SHIFT)
            | (U256::from(self.policy.vwap_lookback_seconds) << LOOKBACK_SHIFT)
            | (U256::from(self.policy.vwap_update_interval_seconds) << INTERVAL_SHIFT)
            | U256::from(self.cutoff)
    }

    pub fn policy(self) -> VwapPolicy {
        self.policy
    }

    pub fn cutoff(self) -> u64 {
        self.cutoff
    }

    pub fn start(self) -> u64 {
        self.cutoff - self.policy.vwap_lookback_seconds
    }
}

/// The snapshot required at `block_timestamp`: its window ends at the latest
/// cutoff at or before that time.
pub fn get_vwap_snapshot_id(block_timestamp: u64, policy: &VwapPolicy) -> Result<VwapSnapshotId> {
    policy.validate()?;
    let cutoff = block_timestamp - block_timestamp % policy.vwap_update_interval_seconds;
    VwapSnapshotId::new(*policy, cutoff)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 1_753_228_800;
    const HOUR: u64 = 3_600;

    fn policy(vwap_lookback_seconds: u64, vwap_update_interval_seconds: u64) -> VwapPolicy {
        VwapPolicy {
            policy_version: 1,
            vwap_lookback_seconds,
            vwap_update_interval_seconds,
        }
    }

    fn window(timestamp: u64, policy: VwapPolicy) -> (u64, u64) {
        let snapshot = get_vwap_snapshot_id(timestamp, &policy).unwrap();
        (snapshot.start(), snapshot.cutoff())
    }

    #[test]
    fn the_default_policy_selects_the_eight_hours_before_the_last_whole_hour() {
        let at = |h: u64, m: u64, s: u64| DAY + h * HOUR + m * 60 + s;
        let default = DEFAULT_VWAP_POLICY;
        assert_eq!(window(at(10, 37, 0), default), (at(2, 0, 0), at(10, 0, 0)));
        assert_eq!(window(at(10, 59, 59), default), (at(2, 0, 0), at(10, 0, 0)));
        assert_eq!(window(at(11, 0, 0), default), (at(3, 0, 0), at(11, 0, 0)));
        assert_eq!(window(at(15, 58, 0), default), (at(7, 0, 0), at(15, 0, 0)));
        assert_eq!(window(at(0, 20, 0), default), (DAY - 8 * HOUR, DAY));
    }

    #[test]
    fn lookback_and_cadence_are_independent_parameters() {
        let at_10_37 = DAY + 10 * HOUR + 37 * 60;
        assert_eq!(
            window(at_10_37, policy(21_600, HOUR)),
            (DAY + 4 * HOUR, DAY + 10 * HOUR)
        );
        assert_eq!(
            window(at_10_37, policy(28_800, 1_800)),
            (DAY + 2 * HOUR + 1_800, DAY + 10 * HOUR + 1_800)
        );
    }

    #[test]
    fn unsupported_policies_and_pre_epoch_windows_are_rejected() {
        for bad in [
            policy(0, HOUR),
            policy(28_800, 0),
            policy(28_800, 7_000),
            policy(30_000, HOUR),
            policy(MAX_SNAPSHOT_RETENTION_SECONDS + HOUR, HOUR),
        ] {
            assert!(get_vwap_snapshot_id(DAY, &bad).is_err(), "{bad:?}");
        }
        assert!(get_vwap_snapshot_id(7 * HOUR, &DEFAULT_VWAP_POLICY).is_err());
    }

    #[test]
    fn a_snapshot_id_round_trips_and_names_its_policy() {
        let snapshot = get_vwap_snapshot_id(DAY + 10 * HOUR, &DEFAULT_VWAP_POLICY).unwrap();
        let word = snapshot.to_u256();
        assert_eq!(VwapSnapshotId::from_u256(word).unwrap(), snapshot);
        assert_eq!(snapshot.policy(), DEFAULT_VWAP_POLICY);

        let next_version = VwapPolicy {
            policy_version: 2,
            ..DEFAULT_VWAP_POLICY
        };
        let other = get_vwap_snapshot_id(DAY + 10 * HOUR, &next_version).unwrap();
        assert_eq!(other.cutoff(), snapshot.cutoff());
        assert_ne!(other.to_u256(), word);
    }

    #[test]
    fn malformed_snapshot_words_are_rejected() {
        let word = get_vwap_snapshot_id(DAY + 10 * HOUR, &DEFAULT_VWAP_POLICY)
            .unwrap()
            .to_u256();
        for bad in [
            word | (U256::ONE << PACKED_BITS),
            word + U256::ONE,
            word & !(U256::from(u32::MAX) << INTERVAL_SHIFT),
            U256::ZERO,
        ] {
            assert!(VwapSnapshotId::from_u256(bad).is_err(), "{bad:#x}");
        }
    }
}
