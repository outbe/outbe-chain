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
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::constants::MIN_ACTIVATION_BUFFER;
use crate::errors::UpdateError;
use crate::version::{
    protocol_version_major, protocol_version_minor, try_parse_protocol_version, ProtocolVersion,
};

/// Vote action that revokes one node's active enclave binding.
pub const REVOKE_ENCLAVE_BINDING_ACTION_V1: &str = "revoke_enclave_binding_v1";

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
        }
    }

    pub fn from_value(payload: &Value) -> std::result::Result<Self, UpdateError> {
        serde_json::from_value(payload.clone()).map_err(|_| UpdateError::InvalidPayload)
    }

    pub fn protocol_version(&self) -> std::result::Result<ProtocolVersion, UpdateError> {
        Self::parse_version(&self.version)
    }

    pub fn validate(&self, current_height: u64) -> std::result::Result<(), UpdateError> {
        let version = self.protocol_version()?;
        if version.is_zero() {
            return Err(UpdateError::InvalidVersion);
        }
        let min_activation = current_height.saturating_add(MIN_ACTIVATION_BUFFER);
        if self.activation_height < min_activation {
            return Err(UpdateError::HeightInPast);
        }
        self.validate_measurement_upgrade()?;
        if self.mrenclave.is_some() && self.activation_height <= current_height {
            return Err(UpdateError::HeightInPast);
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
) -> std::result::Result<(), UpdateError> {
    ScheduleUpdatePayload::from_value(payload)?.validate(current_height)
}

fn parse_canonical_b256(value: &str) -> std::result::Result<B256, &'static str> {
    let Some(hex_digits) = value.strip_prefix("0x") else {
        return Err("expected 0x-prefixed 32-byte hex string");
    };
    if hex_digits.len() != 64 || !hex_digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("expected canonical 32-byte hex string");
    }
    value.parse::<B256>().map_err(|_| "invalid B256 hex string")
}

fn deserialize_canonical_b256<'de, D>(deserializer: D) -> std::result::Result<B256, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    parse_canonical_b256(&value).map_err(serde::de::Error::custom)
}

fn deserialize_optional_canonical_b256<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<B256>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<String>::deserialize(deserializer)? {
        None => Ok(None),
        Some(value) if value.is_empty() => Ok(None),
        Some(value) => parse_canonical_b256(&value)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// JSON payload for revoking a single node's active enclave binding via vote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeEnclaveBindingPayloadV1 {
    pub action: String,
    #[serde(alias = "nodeIdHash", deserialize_with = "deserialize_canonical_b256")]
    pub node_id_hash: B256,
    #[serde(
        default,
        alias = "bindingId",
        deserialize_with = "deserialize_optional_canonical_b256",
        skip_serializing_if = "Option::is_none"
    )]
    pub binding_id: Option<B256>,
}

impl RevokeEnclaveBindingPayloadV1 {
    pub fn from_value(payload: &Value) -> std::result::Result<Self, UpdateError> {
        let decoded: Self =
            serde_json::from_value(payload.clone()).map_err(|_| UpdateError::InvalidPayload)?;
        decoded.validate()?;
        Ok(decoded)
    }

    pub fn validate(&self) -> std::result::Result<(), UpdateError> {
        if self.action != REVOKE_ENCLAVE_BINDING_ACTION_V1
            || self.node_id_hash.is_zero()
            || self.binding_id.is_some_and(|id| id.is_zero())
        {
            return Err(UpdateError::InvalidPayload);
        }
        Ok(())
    }
}

/// Vote payload accepted by [`crate::vote_target::UpdateVoteTarget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateVotePayload {
    ScheduleUpdate(ScheduleUpdatePayload),
    RevokeEnclaveBindingV1(RevokeEnclaveBindingPayloadV1),
}

impl UpdateVotePayload {
    pub fn from_value(payload: &Value) -> std::result::Result<Self, UpdateError> {
        if payload
            .as_object()
            .is_some_and(|obj| obj.contains_key("action"))
        {
            RevokeEnclaveBindingPayloadV1::from_value(payload).map(Self::RevokeEnclaveBindingV1)
        } else {
            ScheduleUpdatePayload::from_value(payload).map(Self::ScheduleUpdate)
        }
    }

    pub fn validate(&self, current_height: u64) -> std::result::Result<(), UpdateError> {
        match self {
            Self::ScheduleUpdate(payload) => payload.validate(current_height),
            Self::RevokeEnclaveBindingV1(payload) => payload.validate(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode_protocol_version;
    use alloy_primitives::B256;

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
            decoded.validate(100).unwrap();
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
    fn measurement_payload_roundtrips_without_external_policy_and_requires_notice() {
        let current = 100;
        let mut value = payload(current + MIN_ACTIVATION_BUFFER);
        value.mrenclave = Some(B256::repeat_byte(1));
        value.validate(current).unwrap();
        assert_eq!(value.validate(current + 1), Err(UpdateError::HeightInPast));
        let encoded = serde_json::to_string(&value).unwrap();
        assert!(!encoded.contains("predecessorTeePolicyHash"));
        assert!(!encoded.contains("teePolicy"));
        assert_eq!(
            serde_json::from_str::<ScheduleUpdatePayload>(&encoded).unwrap(),
            value
        );
        value.mrenclave = Some(B256::ZERO);
        assert_eq!(value.validate(current), Err(UpdateError::InvalidTeePolicy));
    }

    #[test]
    fn activation_requires_the_standard_notice_period() {
        let current = 100;
        let just_under = current + MIN_ACTIVATION_BUFFER - 1;
        assert!(matches!(
            payload(just_under).validate(current),
            Err(UpdateError::HeightInPast)
        ));
        assert!(payload(current + MIN_ACTIVATION_BUFFER)
            .validate(current)
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
            validate_schedule_update_json(&value, 0).unwrap_err(),
            UpdateError::InvalidVersion
        );
    }

    fn canonical_node() -> String {
        format!("{:#x}", B256::repeat_byte(0x11))
    }

    #[test]
    fn revoke_payload_accepts_binding_id_and_its_absence() {
        let node = canonical_node();
        let with_binding = serde_json::json!({
            "action": REVOKE_ENCLAVE_BINDING_ACTION_V1,
            "node_id_hash": node,
            "binding_id": format!("{:#x}", B256::repeat_byte(0x22)),
        });
        match UpdateVotePayload::from_value(&with_binding).unwrap() {
            UpdateVotePayload::RevokeEnclaveBindingV1(payload) => {
                assert_eq!(payload.node_id_hash, B256::repeat_byte(0x11));
                assert_eq!(payload.binding_id, Some(B256::repeat_byte(0x22)));
                payload.validate().unwrap();
            }
            UpdateVotePayload::ScheduleUpdate(_) => panic!("expected revocation payload"),
        }

        for binding in [
            None,
            Some(serde_json::json!(null)),
            Some(serde_json::json!("")),
        ] {
            let mut value = serde_json::json!({
                "action": REVOKE_ENCLAVE_BINDING_ACTION_V1,
                "nodeIdHash": node,
            });
            if let Some(binding) = binding {
                value["bindingId"] = binding;
            }
            let decoded = RevokeEnclaveBindingPayloadV1::from_value(&value).unwrap();
            assert_eq!(decoded.binding_id, None);
            assert_eq!(decoded.node_id_hash, B256::repeat_byte(0x11));
        }
    }

    #[test]
    fn revoke_payload_rejects_noncanonical_identity_and_shape() {
        let node = canonical_node();
        let bare = node.trim_start_matches("0x");
        let cases = [
            serde_json::json!({
                "action": REVOKE_ENCLAVE_BINDING_ACTION_V1,
                "node_id_hash": bare,
            }),
            serde_json::json!({
                "action": REVOKE_ENCLAVE_BINDING_ACTION_V1,
                "node_id_hash": [1, 2],
            }),
            serde_json::json!({
                "action": REVOKE_ENCLAVE_BINDING_ACTION_V1,
                "node_id_hash": format!("{:#x}", B256::ZERO),
            }),
            serde_json::json!({
                "action": REVOKE_ENCLAVE_BINDING_ACTION_V1,
                "node_id_hash": node,
                "binding_id": format!("{:#x}", B256::ZERO),
            }),
            serde_json::json!({
                "action": REVOKE_ENCLAVE_BINDING_ACTION_V1,
                "node_id_hash": node,
                "binding_id": "  ",
            }),
            serde_json::json!({
                "action": "retire_enclave",
                "node_id_hash": node,
            }),
            serde_json::json!({
                "action": REVOKE_ENCLAVE_BINDING_ACTION_V1,
                "node_id_hash": node,
                "extra": true,
            }),
        ];
        for case in cases {
            assert_eq!(
                RevokeEnclaveBindingPayloadV1::from_value(&case).unwrap_err(),
                UpdateError::InvalidPayload,
                "{case}"
            );
            assert_eq!(
                UpdateVotePayload::from_value(&case).unwrap_err(),
                UpdateError::InvalidPayload,
                "{case}"
            );
        }
    }

    #[test]
    fn update_vote_payload_keeps_schedule_updates_without_action() {
        let value = serde_json::json!({"version":"1.2","activationHeight":1000,"info":""});
        let decoded = UpdateVotePayload::from_value(&value).unwrap();
        assert!(matches!(decoded, UpdateVotePayload::ScheduleUpdate(_)));
        decoded.validate(0).unwrap();
    }
}
