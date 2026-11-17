//! Finalized read-only TEE chain state and separately observed local health.

use std::{io::Read, os::unix::fs::OpenOptionsExt, path::Path};

use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use eyre::Result;
use outbe_primitives::tee_attestation_v1::{AttestationMode, EnclaveInitializationManifestV1};
use outbe_tee::registry_view::{read_finalized_registry_view_v1, NodeBindingSelectorV1};
use serde::Serialize;
use serde_json::{json, Value};

use super::{args::StatusArgs, rpc::CliFinalityRpc};
use crate::{
    abi::{ITeeRegistry, TEE_REGISTRY_ADDR},
    errors::TeeStatusError,
    rpc::Rpc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum RenewalAlertLevelV1 {
    Healthy,
    Warning,
    Critical,
}

pub(super) async fn read_status(client: &(impl Rpc + Sync), args: &StatusArgs) -> Result<Value> {
    if args.critical_blocks > args.warning_blocks {
        return Err(TeeStatusError::CriticalMarginExceedsWarning.into());
    }
    let manifest = args
        .node_data_dir
        .as_deref()
        .map(read_public_manifest)
        .transpose()?;
    let selector = manifest.as_ref().map_or(
        NodeBindingSelectorV1::Validator(Address::ZERO),
        |manifest| NodeBindingSelectorV1::NodeHost(manifest.node_id.reth_p2p_public),
    );
    let view = read_finalized_registry_view_v1(&CliFinalityRpc(client), &selector).await?;
    if manifest
        .as_ref()
        .is_some_and(|manifest| manifest.network_binding() != view.policy.network_binding())
    {
        return Err(TeeStatusError::ManifestChainIdentityMismatch.into());
    }
    let tag = format!("0x{:x}", view.schedule.finalized_height);
    let upgrade = client
        .eth_call_at(
            TEE_REGISTRY_ADDR,
            &ITeeRegistry::enclaveUpgradeV1Call {}.abi_encode(),
            &tag,
        )
        .await?;
    let upgrade = ITeeRegistry::enclaveUpgradeV1Call::abi_decode_returns(&upgrade)?;
    let staged = client
        .eth_call_at(
            TEE_REGISTRY_ADDR,
            &ITeeRegistry::stagedSuccessorPolicyV1Call {}.abi_encode(),
            &tag,
        )
        .await?;
    let staged = ITeeRegistry::stagedSuccessorPolicyV1Call::abi_decode_returns(&staged)?;
    let consensus = client.outbe_consensus_status().await?;
    let enclave = consensus
        .get("enclave")
        .ok_or(TeeStatusError::MissingEnclaveHealth)?;
    let lease = manifest.as_ref().and(view.binding.as_ref()).map(|binding| {
        let collateral = match view.policy.attestation_mode {
            AttestationMode::DcapRequired => Some(binding.collateral_valid_until),
            AttestationMode::NoAttestationDev => None,
        };
        let safe = lease_is_safe_for_next_freeze(binding.valid_until, collateral, view.schedule.forecast_freeze_timestamp());
        json!({
            "alert": classify_alert(safe, view.schedule.blocks_until_freeze(), args.warning_blocks, args.critical_blocks),
            "validUntil": binding.valid_until,
            "collateralValidUntil": collateral,
            "safeForNextFreeze": safe,
        })
    });
    Ok(json!({
        "schedule": view.schedule,
        "activePolicy": {
            "hash": view.policy.policy_hash()?,
            "canonical": format!("0x{}", hex::encode(view.policy.encode_canonical()?)),
            "attestationMode": view.policy.attestation_mode as u8,
            "activationHeight": view.policy.activation_height,
        },
        "networkEncryptionPublicKey": view.network_encryption_public_key,
        "binding": manifest.as_ref().and(view.binding.as_ref()),
        "lease": lease,
        "upgrade": {
            "proposalId": upgrade.proposalId,
            "activationHeight": upgrade.activationHeight,
            "mrenclave": upgrade.mrenclave,
            "successorPolicyHash": upgrade.successorPolicyHash,
            "predecessorPolicyHash": upgrade.predecessorPolicyHash,
        },
        "stagedSuccessor": {
            "exists": staged.exists,
            "proposalId": staged.proposalId,
            "canonical": format!("0x{}", hex::encode(staged.policy)),
        },
        // Local canary health is not a finalized registry admission verdict.
        "enclave": enclave,
    }))
}

// The runtime loader acquires a write lock and reconciles journals; status must not.
fn read_public_manifest(node_data_dir: &Path) -> Result<EnclaveInitializationManifestV1> {
    use outbe_tee::node_host::{NODE_HOST_DIRECTORY_V1, NODE_HOST_MANIFEST_V1};
    let path = node_data_dir
        .join(NODE_HOST_DIRECTORY_V1)
        .join(NODE_HOST_MANIFEST_V1);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(TeeStatusError::ReadPublicManifest)?;
    let metadata = file
        .metadata()
        .map_err(TeeStatusError::ReadPublicManifest)?;
    // Match the runtime's initialization-manifest ceiling, including racing growth.
    if !metadata.is_file() || metadata.len() > 512 {
        return Err(TeeStatusError::InvalidPublicManifestFile.into());
    }
    let mut bytes = Vec::new();
    file.take(513)
        .read_to_end(&mut bytes)
        .map_err(TeeStatusError::ReadPublicManifest)?;
    if bytes.len() > 512 {
        return Err(TeeStatusError::InvalidPublicManifestFile.into());
    }
    EnclaveInitializationManifestV1::decode_canonical(&bytes)
        .map_err(|error| TeeStatusError::DecodePublicManifest(error).into())
}

fn lease_is_safe_for_next_freeze(
    valid_until: u64,
    collateral_valid_until: Option<u64>,
    forecast_freeze_timestamp: u64,
) -> bool {
    valid_until > forecast_freeze_timestamp
        && collateral_valid_until.is_none_or(|deadline| deadline > forecast_freeze_timestamp)
}

fn classify_alert(
    safe_for_next_freeze: bool,
    blocks_until_freeze: u64,
    warning_blocks: u64,
    critical_blocks: u64,
) -> RenewalAlertLevelV1 {
    if safe_for_next_freeze {
        RenewalAlertLevelV1::Healthy
    } else if blocks_until_freeze <= critical_blocks {
        RenewalAlertLevelV1::Critical
    } else if blocks_until_freeze <= warning_blocks {
        RenewalAlertLevelV1::Warning
    } else {
        RenewalAlertLevelV1::Healthy
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod rpc_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alerts_are_unsafe_and_freeze_relative() {
        assert_eq!(
            classify_alert(true, 0, 600, 120),
            RenewalAlertLevelV1::Healthy
        );
        assert_eq!(
            classify_alert(false, 601, 600, 120),
            RenewalAlertLevelV1::Healthy
        );
        assert_eq!(
            classify_alert(false, 600, 600, 120),
            RenewalAlertLevelV1::Warning
        );
        assert_eq!(
            classify_alert(false, 120, 600, 120),
            RenewalAlertLevelV1::Critical
        );
    }

    #[test]
    fn direct_dev_safety_has_no_fake_intel_collateral_deadline() {
        assert!(lease_is_safe_for_next_freeze(201, None, 200));
        assert!(!lease_is_safe_for_next_freeze(200, None, 200));
        assert!(lease_is_safe_for_next_freeze(201, Some(201), 200));
        assert!(!lease_is_safe_for_next_freeze(201, Some(200), 200));
    }
}
