//! Vote payload encoding for scheduled updates.
//!
//! JSON schema:
//! ```json
//! {"version":"1.2", "activationHeight":12345, "info":"notes", "mrenclave":"0x<64 hexadecimal digits>"}
//! ```
//!
//! `version` is a `"major.minor"` string (no `v` prefix). The decoder rejects raw
//! numeric JSON values and undotted version strings. An absent, null or empty
//! `mrenclave` means no enclave update. The decoder rejects unknown fields.

use alloy_primitives::B256;
use outbe_ocompregistry::{poc_schema_limits, OcompSuccessorV1};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::constants::min_activation_buffer;
use crate::errors::UpdateError;
use crate::version::{
    protocol_version_major, protocol_version_minor, try_parse_protocol_version, ProtocolVersion,
};

/// JSON payload for scheduling a protocol update via vote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScheduleUpdatePayload {
    pub version: String,
    pub activation_height: u64,
    #[serde(default)]
    pub info: String,
    /// A nonempty measurement opts into enclave upgrade and hard retirement.
    #[serde(
        default,
        deserialize_with = "deserialize_mrenclave",
        skip_serializing_if = "Option::is_none"
    )]
    pub mrenclave: Option<B256>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocomp_successor: Option<String>,
}

/// Empty optional UI fields do not request a code change. A supplied measurement
/// must still be a valid 32-byte hash. Payload validation rejects zero.
fn deserialize_mrenclave<'de, D>(deserializer: D) -> Result<Option<B256>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<String>::deserialize(deserializer)? {
        None => Ok(None),
        Some(value) if value.trim().is_empty() => Ok(None),
        Some(value) => value.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

impl ScheduleUpdatePayload {
    pub fn new(version: ProtocolVersion, activation_height: u64, info: impl Into<String>) -> Self {
        Self {
            version: Self::format_version(version),
            activation_height,
            info: info.into(),
            mrenclave: None,
            ocomp_successor: None,
        }
    }

    /// Adds one exact predecessor-bound OCOMP successor to this Update.
    pub fn with_ocomp_successor(
        mut self,
        successor: &OcompSuccessorV1,
    ) -> std::result::Result<Self, UpdateError> {
        let canonical = successor
            .encode_canonical(&poc_schema_limits())
            .map_err(|_| UpdateError::InvalidOcompSuccessor)?;
        self.ocomp_successor = Some(hex::encode(canonical));
        Ok(self)
    }

    pub fn from_value(payload: &Value) -> std::result::Result<Self, UpdateError> {
        serde_json::from_value(payload.clone()).map_err(|_| UpdateError::InvalidPayload)
    }

    pub fn protocol_version(&self) -> std::result::Result<ProtocolVersion, UpdateError> {
        Self::parse_version(&self.version)
    }

    pub fn ocomp_successor(&self) -> std::result::Result<Option<OcompSuccessorV1>, UpdateError> {
        let Some(encoded) = self.ocomp_successor.as_deref() else {
            return Ok(None);
        };
        let maximum_hex_len = poc_schema_limits()
            .codec
            .max_allocation_bytes
            .checked_mul(2)
            .ok_or(UpdateError::InvalidOcompSuccessor)?;
        if encoded.is_empty() || encoded.len() > maximum_hex_len || encoded.len() % 2 != 0 {
            return Err(UpdateError::InvalidOcompSuccessor);
        }
        if !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(UpdateError::InvalidOcompSuccessor);
        }
        let canonical = hex::decode(encoded).map_err(|_| UpdateError::InvalidOcompSuccessor)?;
        OcompSuccessorV1::decode_canonical(&canonical, &poc_schema_limits())
            .map(Some)
            .map_err(|_| UpdateError::InvalidOcompSuccessor)
    }

    pub fn validate(
        &self,
        current_height: u64,
        chain_id: u64,
    ) -> std::result::Result<(), UpdateError> {
        let version = self.protocol_version()?;
        if version.is_zero() {
            return Err(UpdateError::InvalidVersion);
        }
        let min_activation = current_height.saturating_add(min_activation_buffer(chain_id));
        if self.activation_height < min_activation {
            return Err(UpdateError::HeightInPast);
        }
        self.validate_measurement_upgrade()?;
        if self.mrenclave.is_some() && self.activation_height <= current_height {
            return Err(UpdateError::HeightInPast);
        }
        if let Some(successor) = self.ocomp_successor()? {
            if successor.authority.request_profile.chain_id != chain_id {
                return Err(UpdateError::OcompSuccessorChainIdentityMismatch);
            }
            if successor.activation_height != self.activation_height {
                return Err(UpdateError::OcompSuccessorActivationMismatch);
            }
        }
        Ok(())
    }

    pub fn validate_measurement_upgrade(&self) -> std::result::Result<(), UpdateError> {
        if self
            .mrenclave
            .is_some_and(|measurement| measurement.is_zero())
        {
            return Err(UpdateError::InvalidTeePolicy);
        }
        Ok(())
    }

    /// Formats a protocol version as the vote-payload string `"major.minor"`.
    fn format_version(version: ProtocolVersion) -> String {
        format!(
            "{}.{}",
            protocol_version_major(version),
            protocol_version_minor(version)
        )
    }

    /// Parses a vote-payload `"major.minor"` version string.
    fn parse_version(version: &str) -> std::result::Result<ProtocolVersion, UpdateError> {
        // Require dotted major.minor form. Reject raw numeric strings like "65538".
        if !version.contains('.') {
            return Err(UpdateError::InvalidPayload);
        }
        try_parse_protocol_version(version).map_err(|_| UpdateError::InvalidVersion)
    }
}

/// Encodes update fields into a vote JSON payload string.
pub fn encode_schedule_update_json(
    version: ProtocolVersion,
    activation_height: u64,
    info: &str,
) -> String {
    serde_json::to_string(&ScheduleUpdatePayload::new(
        version,
        activation_height,
        info,
    ))
    .expect("schedule update payload JSON should serialize")
}

/// Decodes a vote JSON payload into update fields.
pub fn decode_schedule_update_json(
    payload: &Value,
) -> std::result::Result<(ProtocolVersion, u64, String), UpdateError> {
    let decoded = ScheduleUpdatePayload::from_value(payload)?;
    Ok((
        decoded.protocol_version()?,
        decoded.activation_height,
        decoded.info,
    ))
}

/// Validates structural update JSON fields and activation-height buffer.
pub fn validate_schedule_update_json(
    payload: &Value,
    current_height: u64,
    chain_id: u64,
) -> std::result::Result<(), UpdateError> {
    ScheduleUpdatePayload::from_value(payload)?.validate(current_height, chain_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::MIN_ACTIVATION_BUFFER;
    use crate::encode_protocol_version;
    use alloy_primitives::B256;
    const LOCALNET_CHAIN_ID: u64 = 54_322_345;
    const OTHER_CHAIN_ID: u64 = 1;

    fn payload(activation_height: u64) -> ScheduleUpdatePayload {
        ScheduleUpdatePayload::new(ProtocolVersion::from(2), activation_height, "notes")
    }

    #[test]
    fn optional_measurement_accepts_absent_null_and_empty() {
        for field in [
            None,
            Some(serde_json::json!(null)),
            Some(serde_json::json!("")),
            Some(serde_json::json!("  ")),
        ] {
            let mut json = serde_json::json!({"version":"1.2", "activationHeight":1000});
            if let Some(field) = field {
                json["mrenclave"] = field;
            }
            let decoded = ScheduleUpdatePayload::from_value(&json).unwrap();
            assert_eq!(decoded.mrenclave, None);
            decoded.validate(100, OTHER_CHAIN_ID).unwrap();
        }
    }

    #[test]
    fn removed_policy_fields_are_rejected() {
        for field in ["teePolicy", "predecessorTeePolicyHash", "policyHash"] {
            let mut value = serde_json::json!({"version":"1.2", "activationHeight":1000});
            value[field] = serde_json::json!("0x01");
            assert_eq!(
                ScheduleUpdatePayload::from_value(&value).unwrap_err(),
                UpdateError::InvalidPayload
            );
        }
    }

    #[test]
    fn nonempty_measurement_must_be_valid() {
        for value in [
            serde_json::json!("0x"),
            serde_json::json!("not-a-hash"),
            serde_json::json!(123),
        ] {
            let json =
                serde_json::json!({"version":"1.2", "activationHeight":1000, "mrenclave":value});
            assert!(ScheduleUpdatePayload::from_value(&json).is_err());
        }
    }

    #[test]
    fn measurement_payload_roundtrips_without_external_policy_and_requires_future_height() {
        let mut value = payload(100);
        value.mrenclave = Some(B256::repeat_byte(1));
        value.validate(99, LOCALNET_CHAIN_ID).unwrap();
        assert!(value.validate(100, LOCALNET_CHAIN_ID).is_err());
        let encoded = serde_json::to_string(&value).unwrap();
        assert!(!encoded.contains("predecessorTeePolicyHash"));
        assert!(!encoded.contains("teePolicy"));
        assert_eq!(
            serde_json::from_str::<ScheduleUpdatePayload>(&encoded).unwrap(),
            value
        );
        value.mrenclave = Some(B256::ZERO);
        assert!(value.validate(99, LOCALNET_CHAIN_ID).is_err());
    }

    #[test]
    fn localnet_allows_immediate_activation() {
        // buffer is 0 on localnet: activation at the current height is accepted.
        assert!(payload(100).validate(100, LOCALNET_CHAIN_ID).is_ok());
    }

    #[test]
    fn other_chains_still_require_the_buffer() {
        let current = 100;
        let just_under = current + MIN_ACTIVATION_BUFFER - 1;
        assert!(matches!(
            payload(just_under).validate(current, OTHER_CHAIN_ID),
            Err(UpdateError::HeightInPast)
        ));
        assert!(payload(current + MIN_ACTIVATION_BUFFER)
            .validate(current, OTHER_CHAIN_ID)
            .is_ok());
    }

    #[test]
    fn encode_decode_roundtrip_major_minor_string() {
        let version = encode_protocol_version(1, 2);
        let json = encode_schedule_update_json(version, 12345, "notes");
        assert!(json.contains(r#""version":"1.2""#), "json={json}");

        let value: Value = serde_json::from_str(&json).unwrap();
        let (decoded, height, info) = decode_schedule_update_json(&value).unwrap();
        assert_eq!(decoded, version);
        assert_eq!(height, 12345);
        assert_eq!(info, "notes");
    }

    #[test]
    fn rejects_numeric_version_json() {
        let value: Value =
            serde_json::from_str(r#"{"version":65538,"activationHeight":1000,"info":""}"#).unwrap();
        assert_eq!(
            decode_schedule_update_json(&value).unwrap_err(),
            UpdateError::InvalidPayload
        );
    }

    #[test]
    fn rejects_undotted_version_string() {
        let value: Value =
            serde_json::from_str(r#"{"version":"65538","activationHeight":1000,"info":""}"#)
                .unwrap();
        assert_eq!(
            decode_schedule_update_json(&value).unwrap_err(),
            UpdateError::InvalidPayload
        );
    }

    #[test]
    fn rejects_zero_major_minor_version() {
        let value: Value =
            serde_json::from_str(r#"{"version":"0.0","activationHeight":1000,"info":""}"#).unwrap();
        assert_eq!(
            validate_schedule_update_json(&value, 0, LOCALNET_CHAIN_ID).unwrap_err(),
            UpdateError::InvalidVersion
        );
    }
}
