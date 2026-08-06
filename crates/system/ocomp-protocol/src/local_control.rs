//! Local OCOMP process identity and service-user checks.
//!
//! OCOMP runtime transport is public node RPC, Axum registration, Worker Salvo
//! observability and ZeroMQ over loopback TCP. This module is intentionally
//! transport-free: it retains only fixed chain identity and local process
//! ownership helpers shared by the OCOMP binaries.

use std::fs;

use alloy_primitives::B256;
use thiserror::Error;

use crate::SchemaLimits;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointIdentity {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub boot_nonce: B256,
    pub protocol_bundle_hash: B256,
}

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("local process identity I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("local service user {0} does not exist in /etc/passwd")]
    UnknownUser(String),
    #[error("process effective uid {actual} does not match service user {user} uid {expected}")]
    EffectiveUserMismatch {
        user: String,
        expected: u32,
        actual: u32,
    },
}

/// Compile ceilings generated for the PoC. They do not arm a fork or select a
/// bundle; callers must still supply one exact [`EndpointIdentity`].
#[must_use]
pub fn poc_schema_limits() -> SchemaLimits {
    crate::profile::poc_schema_limits()
}

pub fn effective_uid() -> Result<u32, ControlError> {
    Ok(rustix::process::geteuid().as_raw())
}

pub fn uid_for_user(user: &str) -> Result<u32, ControlError> {
    let passwd = fs::read_to_string("/etc/passwd")?;
    passwd
        .lines()
        .filter_map(parse_passwd_entry)
        .find_map(|(name, uid)| (name == user).then_some(uid))
        .ok_or_else(|| ControlError::UnknownUser(user.to_owned()))
}

pub fn effective_user_name() -> Result<String, ControlError> {
    let uid = effective_uid()?;
    let passwd = fs::read_to_string("/etc/passwd")?;
    passwd
        .lines()
        .filter_map(parse_passwd_entry)
        .find_map(|(name, candidate)| (candidate == uid).then_some(name.to_owned()))
        .ok_or_else(|| ControlError::UnknownUser(format!("uid:{uid}")))
}

pub fn require_effective_user(user: &str) -> Result<u32, ControlError> {
    let expected = uid_for_user(user)?;
    require_effective_uid_for(expected, user.to_owned())
}

pub fn require_effective_uid(expected: u32) -> Result<u32, ControlError> {
    require_effective_uid_for(expected, format!("uid:{expected}"))
}

fn require_effective_uid_for(expected: u32, user: String) -> Result<u32, ControlError> {
    let actual = effective_uid()?;
    if expected != actual {
        return Err(ControlError::EffectiveUserMismatch {
            user,
            expected,
            actual,
        });
    }
    Ok(actual)
}

fn parse_passwd_entry(line: &str) -> Option<(&str, u32)> {
    let mut fields = line.split(':');
    let name = fields.next()?;
    fields.next()?;
    let uid = fields.next()?.parse().ok()?;
    Some((name, uid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_uid_reports_the_current_process_identity() {
        assert_eq!(
            effective_uid().unwrap(),
            rustix::process::geteuid().as_raw()
        );
    }

    #[test]
    fn current_effective_uid_is_accepted() {
        let uid = effective_uid().unwrap();
        assert_eq!(require_effective_uid(uid).unwrap(), uid);
    }
}
