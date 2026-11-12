//! Neutral scheduled-update records shared by the update crate and its callers.

use alloy_primitives::U256;

use outbe_primitives::protocol_version::ProtocolVersion;

/// `status` byte is not a known scheduled-update lifecycle value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidScheduledUpdateStatus;

/// Lifecycle status of a scheduled update (`IUpdate.ScheduledUpdateStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScheduledUpdateStatus {
    Scheduled = 0,
    Activated = 1,
    Canceled = 2,
}

impl ScheduledUpdateStatus {
    /// Decode the stored status byte.
    pub const fn from_u8(value: u8) -> Result<Self, InvalidScheduledUpdateStatus> {
        match value {
            0 => Ok(Self::Scheduled),
            1 => Ok(Self::Activated),
            2 => Ok(Self::Canceled),
            _ => Err(InvalidScheduledUpdateStatus),
        }
    }

    /// Encode the status as its stored byte.
    pub const fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Materialized scheduled update read from storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledUpdateInfo {
    pub proposal_id: U256,
    pub version: ProtocolVersion,
    pub activation_height: u64,
    pub info: String,
    pub status: ScheduledUpdateStatus,
}
