//! Revokes the active enclave binding of one node.

use alloy_primitives::{B256, U256};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_system_common::vote_target::TargetExecutionOutcome;
use outbe_teeregistry::TeeRegistry;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::UpdateError;

/// Vote action that revokes one node's active enclave binding.
pub const REVOKE_ENCLAVE_BINDING_ACTION_V1: &str = "revoke_enclave_binding_v1";

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

pub(super) fn execute_approved_revocation(
    ctx: &BlockRuntimeContext,
    proposal_id: U256,
    decoded: &RevokeEnclaveBindingPayloadV1,
) -> Result<TargetExecutionOutcome> {
    let mut conflict = None;
    let outcome = ctx.with_checkpoint(|| {
        let mut tee_registry = TeeRegistry::new(ctx.storage.clone());
        match tee_registry.revoke_enclave_binding_classified_v1(
            decoded.node_id_hash,
            decoded.binding_id,
            proposal_id,
        )? {
            Ok(_binding) => Ok(()),
            Err(err) => {
                conflict = Some(err);
                Err(err.into_error().into())
            }
        }
    });
    if let Some(conflict) = conflict {
        return Ok(TargetExecutionOutcome::Error {
            reason: alloy_primitives::hex::encode_prefixed(conflict.abi_encode_revert()),
        });
    }
    outcome.map(|()| TargetExecutionOutcome::Applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::UpdateVotePayload;

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
            _ => panic!("expected revocation payload"),
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
}
