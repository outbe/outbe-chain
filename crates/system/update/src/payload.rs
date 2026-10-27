//! Strict live Update actions derive activation height from Vote's three windows.
//! Historical schedules retain their explicit height for stored records and direct fixtures.

use alloy_primitives::{B256, U256};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::constants::MIN_ACTIVATION_BUFFER;
use crate::errors::UpdateError;
use crate::version::{
    protocol_version_major, protocol_version_minor, try_parse_protocol_version, ProtocolVersion,
};

/// Live protocol plan. Callers cannot choose any phase boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdatePlanPayload {
    pub version: String,
    #[serde(default)]
    pub info: String,
    #[serde(
        default,
        deserialize_with = "deserialize_mrenclave",
        skip_serializing_if = "Option::is_none"
    )]
    pub mrenclave: Option<B256>,
}

impl UpdatePlanPayload {
    pub fn new(version: ProtocolVersion, info: impl Into<String>) -> Self {
        Self {
            version: ScheduleUpdatePayload::format_version(version),
            info: info.into(),
            mrenclave: None,
        }
    }

    pub fn protocol_version(&self) -> std::result::Result<ProtocolVersion, UpdateError> {
        ScheduleUpdatePayload::parse_version(&self.version)
    }

    pub fn validate(&self) -> std::result::Result<(), UpdateError> {
        if self.protocol_version()?.is_zero() {
            return Err(UpdateError::InvalidVersion);
        }
        if self
            .mrenclave
            .is_some_and(|measurement| measurement.is_zero())
        {
            return Err(UpdateError::InvalidTeePolicy);
        }
        Ok(())
    }
}

/// Strict, mutually exclusive live primary/cancel/replacement action shapes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UpdateVoteAction {
    Primary(UpdatePlanPayload),
    Cancel(UpdateCancelPayload),
    Replace(UpdateReplacementPayload),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateCancelPayload {
    #[serde(deserialize_with = "deserialize_proposal_id")]
    pub cancel_proposal_id: U256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateReplacementPayload {
    #[serde(deserialize_with = "deserialize_proposal_id")]
    pub replace_proposal_id: U256,
    pub update: UpdatePlanPayload,
}

fn deserialize_proposal_id<'de, D>(deserializer: D) -> Result<U256, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = String::deserialize(deserializer)?;
    let digits = value
        .strip_prefix("0x")
        .filter(|digits| {
            !digits.is_empty()
                && digits.len() <= 64
                && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .ok_or_else(|| D::Error::custom("proposal id must be a 0x-prefixed uint256"))?;
    let id = U256::from_str_radix(digits, 16).map_err(D::Error::custom)?;
    if id.is_zero() {
        return Err(D::Error::custom("proposal id must be nonzero"));
    }
    Ok(id)
}

impl UpdateVoteAction {
    pub fn from_value(value: &Value) -> std::result::Result<Self, UpdateError> {
        serde_json::from_value(value.clone()).map_err(|_| UpdateError::InvalidPayload)
    }

    pub fn plan(&self) -> Option<&UpdatePlanPayload> {
        match self {
            Self::Primary(plan) => Some(plan),
            Self::Replace(replacement) => Some(&replacement.update),
            Self::Cancel(_) => None,
        }
    }
}

pub fn encode_update_plan_json(version: ProtocolVersion, info: &str) -> String {
    serde_json::to_string(&UpdatePlanPayload::new(version, info))
        .expect("update plan JSON should serialize")
}

/// Historical payload for stored proposals and the direct scheduling API, not live Vote admission.
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

/// Encodes a historical stored schedule/direct-API fixture, never a live Vote action.
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

/// Decodes historical schedule fields, preserving their caller-selected height.
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

/// Validates a historical/direct schedule and its activation-height buffer.
pub fn validate_schedule_update_json(
    payload: &Value,
    current_height: u64,
) -> std::result::Result<(), UpdateError> {
    ScheduleUpdatePayload::from_value(payload)?.validate(current_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode_protocol_version;
    use alloy_primitives::B256;

    #[test]
    fn live_actions_are_strict_and_have_no_height_alias() {
        let primary = serde_json::json!({"version":"1.2"});
        let cancel = serde_json::json!({"cancelProposalId":"0x1"});
        let replacement = serde_json::json!({"replaceProposalId":"0x1","update":primary});
        for value in [&primary, &cancel, &replacement] {
            let action = UpdateVoteAction::from_value(value).unwrap();
            if let Some(plan) = action.plan() {
                plan.validate().unwrap();
            }
            assert_eq!(
                serde_json::from_str::<UpdateVoteAction>(&serde_json::to_string(&action).unwrap())
                    .unwrap(),
                action
            );
        }
        for value in [
            serde_json::json!({"version":"1.2","activationHeight":123}),
            serde_json::json!({"cancelProposalId":"0x1","info":"extra"}),
            serde_json::json!({"cancelProposalId":"0x1","replaceProposalId":"0x2","update":primary}),
            serde_json::json!({"replaceProposalId":"0x1","update":{"version":"1.2","activationHeight":123}}),
            serde_json::json!({"replaceProposalId":"0x1","update":{"version":"1.2"},"activationHeight":123}),
        ] {
            assert_eq!(
                UpdateVoteAction::from_value(&value),
                Err(UpdateError::InvalidPayload)
            );
        }
    }

    #[test]
    fn live_measurement_and_version_validation_is_not_weakened() {
        for measurement in [
            serde_json::json!(null),
            serde_json::json!(""),
            serde_json::json!("  "),
        ] {
            let action = UpdateVoteAction::from_value(
                &serde_json::json!({"version":"1.2","mrenclave":measurement}),
            )
            .unwrap();
            assert_eq!(action.plan().unwrap().mrenclave, None);
        }
        let zero = UpdateVoteAction::from_value(
            &serde_json::json!({"version":"1.2","mrenclave":B256::ZERO}),
        )
        .unwrap();
        assert_eq!(
            zero.plan().unwrap().validate(),
            Err(UpdateError::InvalidTeePolicy)
        );
        let zero_version =
            UpdateVoteAction::from_value(&serde_json::json!({"version":"0.0"})).unwrap();
        assert_eq!(
            zero_version.plan().unwrap().validate(),
            Err(UpdateError::InvalidVersion)
        );
        for version in [serde_json::json!(65538), serde_json::json!("65538")] {
            assert!(
                UpdateVoteAction::from_value(&serde_json::json!({"version":version}))
                    .and_then(|action| action.plan().unwrap().validate())
                    .is_err()
            );
        }
        let encoded = encode_update_plan_json(encode_protocol_version(1, 2), "notes");
        let action =
            UpdateVoteAction::from_value(&serde_json::from_str(&encoded).unwrap()).unwrap();
        assert_eq!(
            action.plan().unwrap().protocol_version().unwrap(),
            encode_protocol_version(1, 2)
        );
    }

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

    #[test]
    fn linked_action_ids_require_nonzero_hex_uint256_strings() {
        for id in [
            serde_json::json!(1),
            serde_json::json!("1"),
            serde_json::json!("0x"),
            serde_json::json!("0x0"),
            serde_json::json!("0xgg"),
            serde_json::json!(format!("0x1{}", "0".repeat(64))),
        ] {
            for action in [
                serde_json::json!({"cancelProposalId":id}),
                serde_json::json!({"replaceProposalId":id,"update":{"version":"1.2"}}),
            ] {
                assert_eq!(
                    UpdateVoteAction::from_value(&action),
                    Err(UpdateError::InvalidPayload)
                );
            }
        }
        let maximum = serde_json::json!({"cancelProposalId":format!("0x{:x}",U256::MAX)});
        assert!(matches!(UpdateVoteAction::from_value(&maximum),
            Ok(UpdateVoteAction::Cancel(UpdateCancelPayload { cancel_proposal_id })) if cancel_proposal_id == U256::MAX));
    }
}
