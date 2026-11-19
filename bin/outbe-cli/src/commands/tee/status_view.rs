//! Finalized read-only TEE chain state and separately observed local health.

use std::{
    fs::Metadata,
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use eyre::{bail, Result};
use outbe_primitives::tee_attestation_v1::AttestationMode;
use outbe_simplex::bls::{self, KeyBackend, KeyBackendNameError};
use outbe_tee::registry_view::{read_finalized_registry_view_v1, NodeBindingSelectorV1};
use serde::Serialize;
use serde_json::{json, Value};
use zeroize::Zeroizing;

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
    let selected = binding_selector(args)?;
    let selector = selected
        .clone()
        .unwrap_or(NodeBindingSelectorV1::Validator(Address::ZERO));
    let view = read_finalized_registry_view_v1(&CliFinalityRpc(client), &selector).await?;
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
    let lease = selected.as_ref().and(view.binding.as_ref()).map(|binding| {
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
        "binding": selected.as_ref().and(view.binding.as_ref()),
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

/// `None` keeps the historical network-wide validator-zero selector and hides
/// the binding. A selected host or validator shows the finalized binding.
fn binding_selector(args: &StatusArgs) -> Result<Option<NodeBindingSelectorV1>> {
    let choices = [
        args.node_data_dir.is_some(),
        args.node_id.is_some(),
        args.validator.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if choices > 1 {
        bail!("pass only one of --node-data-dir, --node-id, or --validator");
    }
    if let Some(dir) = args.node_data_dir.as_deref() {
        let backend = reth_discovery_backend(args)?;
        return Ok(Some(NodeBindingSelectorV1::NodeHost(
            node_host_public_from_discovery_secret(dir, &backend)?,
        )));
    }
    if let Some(node_id) = args.node_id {
        return Ok(Some(NodeBindingSelectorV1::NodeHost(node_id)));
    }
    if let Some(validator) = args.validator {
        return Ok(Some(NodeBindingSelectorV1::Validator(validator)));
    }
    Ok(None)
}

/// Same custody selector the node launcher uses for `<datadir>/discovery-secret`.
fn reth_discovery_backend(args: &StatusArgs) -> Result<KeyBackend> {
    if args.reth_key_backend != "encrypted" && args.reth_passphrase.is_some() {
        return Err(TeeStatusError::PassphraseRequiresEncryptedBackend.into());
    }
    let passphrase = args
        .reth_passphrase
        .clone()
        .filter(|passphrase| !passphrase.is_empty());
    bls::parse_key_backend_name(&args.reth_key_backend, passphrase).map_err(|error| match error {
        KeyBackendNameError::MissingPassphrase => TeeStatusError::MissingRethPassphrase.into(),
        KeyBackendNameError::Unknown(name) => TeeStatusError::UnknownRethKeyBackend(name).into(),
    })
}

const DISCOVERY_SECRET_READ_LIMIT: u64 = 4096;

/// The node publishes the compressed SEC1 key from `<datadir>/discovery-secret`
/// as `NodeHostIdentityV1.reth_p2p_public`. Status reads that file once and
/// does not create a key or change its mode.
fn node_host_public_from_discovery_secret(
    node_data_dir: &Path,
    backend: &KeyBackend,
) -> Result<[u8; 33]> {
    let path = node_data_dir.join("discovery-secret");
    let (metadata, raw) = read_discovery_secret_file(&path)?;
    let secret = decode_discovery_secret(&path, &metadata, &raw, backend)?;
    Ok(compressed_reth_p2p_public(&secret)?)
}

fn decode_discovery_secret(
    path: &Path,
    metadata: &Metadata,
    raw: &[u8],
    backend: &KeyBackend,
) -> Result<Zeroizing<[u8; 32]>, TeeStatusError> {
    match backend {
        KeyBackend::Plaintext => {
            if bls::reads_as_protected_custody(raw) {
                return Err(TeeStatusError::invalid_backend_format());
            }
            decode_plaintext_reth_secret(raw)
        }
        // The node loader auto-detects custody and still accepts a mode-0600
        // Reth plaintext file when the selected backend is encrypted or os-level.
        KeyBackend::Encrypted(_) | KeyBackend::OsLevel => {
            ensure_private_custody_mode(path, metadata)?;
            bls::load_secp256k1_secret_from_bytes(raw, backend)
                .map_err(TeeStatusError::from_custody_report)
        }
    }
}

fn read_discovery_secret_file(path: &Path) -> Result<(Metadata, Zeroizing<Vec<u8>>)> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(TeeStatusError::read_discovery_io)?;
    let metadata = file.metadata().map_err(TeeStatusError::read_discovery_io)?;
    if !metadata.file_type().is_file() {
        return Err(TeeStatusError::invalid_not_regular_file().into());
    }
    let mut raw = Zeroizing::new(Vec::new());
    file.take(DISCOVERY_SECRET_READ_LIMIT + 1)
        .read_to_end(&mut raw)
        .map_err(TeeStatusError::read_discovery_io)?;
    if raw.len() > DISCOVERY_SECRET_READ_LIMIT as usize {
        return Err(TeeStatusError::invalid_too_large().into());
    }
    Ok((metadata, raw))
}

fn ensure_private_custody_mode(path: &Path, metadata: &Metadata) -> Result<(), TeeStatusError> {
    let mode = metadata.permissions().mode();
    if mode & 0o077 == 0 {
        return Ok(());
    }
    Err(TeeStatusError::invalid_insecure_mode(path, mode & 0o777))
}

fn decode_plaintext_reth_secret(raw: &[u8]) -> Result<Zeroizing<[u8; 32]>, TeeStatusError> {
    const HEX_LEN: usize = 64;
    if raw.len() != HEX_LEN {
        return Err(TeeStatusError::invalid_plaintext_length());
    }
    let mut secret = Zeroizing::new([0_u8; 32]);
    hex::decode_to_slice(raw, &mut secret[..]).map_err(TeeStatusError::invalid_hex)?;
    Ok(secret)
}

fn compressed_reth_p2p_public(secret: &[u8; 32]) -> Result<[u8; 33], TeeStatusError> {
    let signing =
        k256::ecdsa::SigningKey::from_slice(secret).map_err(TeeStatusError::invalid_secp256k1)?;
    signing
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| TeeStatusError::invalid_compressed_length())
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
