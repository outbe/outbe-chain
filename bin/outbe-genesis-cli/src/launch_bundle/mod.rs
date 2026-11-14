//! Render the per-machine launch bundle that goes with a generated genesis.

pub mod dist;
pub mod scripts;
pub mod systemd;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::Value;
use walkdir::WalkDir;

use outbe_primitives::tee_attestation_v1::{
    AttestationMode, NetworkBindingV1, TeePolicyScheduleV1,
};

use crate::error::GenesisError;
pub use crate::keys::ensure_reth_p2p_secret;
use crate::seed_codec::parse_u64;

pub use self::dist::{build_distribution, deploy_markdown};
pub use self::scripts::{
    enclave_script, feeder_config, feeder_pairs, feeder_pairs_file, feeder_pairs_settings,
    feeder_pairs_table, feeder_script, node_script, radicle_script, start_all_script,
    stop_all_script,
};
pub use self::systemd::{
    caddy_install_script, caddyfile, preflight_script, systemd_unit, tee_renew_script,
    write_systemd_units, UNIT_ROLES,
};

/// Default port numbers shared across validator hosts.
pub const DEFAULT_PORTS: &[(&str, u16)] = &[
    ("reth_p2p_port", 30303),
    ("reth_discv5_port", 31303),
    ("rpc_port", 8545),
    ("authrpc_port", 8551),
    ("metrics_port", 9101),
    ("radicle_port", 8776),
    ("radicle_status_port", 8876),
    ("tee_enclave_port", 17000),
    ("feeder_health_port", 9002),
    ("public_rpc_port", 80),
    ("public_radicle_status_port", 8080),
];

/// Protocol ceiling on the validator set, mirroring the ValidatorSet precompile default.
pub const DEFAULT_MAX_VALIDATORS: u64 = 128;

/// Files required below the install root of a signed SGX release bundle.
pub const SIGNED_ENCLAVE_FILES: &[&str] = &[
    "bin/outbe-tee-enclave",
    "bin/outbe-tee-enclave-launch",
    "gramine/loader",
    "gramine/libpal.so",
    "network-descriptor-v1.bin",
    "outbe-tee-enclave.manifest",
    "outbe-tee-enclave.manifest.sgx",
    "outbe-tee-enclave.sig",
];

struct BundlePaths<'a> {
    output_dir: &'a Path,
    keys_dir: &'a Path,
    base_dir: &'a str,
    remote_keys_dir: &'a str,
}

/// POSIX shell quoting matching Python's `shlex.quote`.
pub fn quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    let is_safe = value.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
            )
    });
    if is_safe {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

/// Resolve a configured port or fall back to [`DEFAULT_PORTS`].
pub fn port_of(config: &Value, name: &str) -> Result<u16, GenesisError> {
    if let Some(val) = config.get(name) {
        let port = parse_u64(val)?;
        return u16::try_from(port)
            .map_err(|_| GenesisError::validation(format!("{name} out of u16 port range")));
    }
    DEFAULT_PORTS
        .iter()
        .find_map(|&(k, v)| (k == name).then_some(v))
        .ok_or_else(|| GenesisError::validation(format!("unknown port key: {name}")))
}

/// Write an executable bash script with `set -euo pipefail`.
pub fn write_script(path: &Path, body: &str) -> Result<(), GenesisError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| GenesisError::io(parent, err))?;
    }
    let content = format!(
        "#!/usr/bin/env bash\nset -euo pipefail\n\n{}\n",
        body.trim()
    );
    fs::write(path, content).map_err(|err| GenesisError::io(path, err))?;
    let mut perms = fs::metadata(path)
        .map_err(|err| GenesisError::io(path, err))?
        .permissions();
    perms.set_mode(perms.mode() | 0o111);
    fs::set_permissions(path, perms).map_err(|err| GenesisError::io(path, err))?;
    Ok(())
}

/// Format an enode host (wrapping IPv6 literals in brackets).
pub fn format_enode_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

/// Determine whether the node should use `--tee-session-mode production-node-host`.
pub fn uses_production_tee_session(config: &Value) -> bool {
    let enclave_sgx = config
        .get("enclave_sgx")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let is_dcap = config
        .get("tee")
        .and_then(|t| t.get("mode"))
        .and_then(Value::as_str)
        == Some("dcap-required");
    enclave_sgx || is_dcap
}

fn direct_dev_network_binding(
    config: &Value,
    genesis_path: &Path,
) -> Result<Option<String>, GenesisError> {
    if uses_production_tee_session(config) {
        return Ok(None);
    }
    let mode = config
        .get("tee")
        .and_then(|tee| tee.get("mode"))
        .and_then(Value::as_str);
    if mode != Some("gramine-direct-dev") {
        return Ok(None);
    }

    let chain_id = parse_u64(
        config
            .get("chain_id")
            .ok_or_else(|| GenesisError::validation("chain_id is required"))?,
    )?;
    let genesis: Value = serde_json::from_slice(
        &fs::read(genesis_path).map_err(|error| GenesisError::io(genesis_path, error))?,
    )?;
    let encoded = genesis
        .pointer("/config/teeAttestationV1/policySchedule")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("genesis has no TEE policy schedule"))?;
    let bytes = hex::decode(encoded.strip_prefix("0x").unwrap_or(encoded)).map_err(|error| {
        GenesisError::validation(format!("invalid TEE policy schedule: {error}"))
    })?;
    let schedule = TeePolicyScheduleV1::decode_canonical(&bytes).map_err(|error| {
        GenesisError::validation(format!("invalid TEE policy schedule: {error}"))
    })?;
    let expected_chain_id = alloy_primitives::U256::from(chain_id).to_be_bytes::<32>();
    if schedule.chain_id != expected_chain_id {
        return Err(GenesisError::validation(
            "genesis TEE policy chain id does not match chain_id",
        ));
    }
    let policy = schedule.active_policy(1).map_err(|error| {
        GenesisError::validation(format!("invalid genesis TEE policy: {error}"))
    })?;
    if policy.attestation_mode != AttestationMode::GramineDirectDev {
        return Err(GenesisError::validation(
            "gramine-direct-dev requires a GramineDirectDev genesis TEE policy",
        ));
    }
    let binding = NetworkBindingV1 {
        chain_id: schedule.chain_id,
        genesis_hash: schedule.genesis_hash,
        attestation_mode: policy.attestation_mode,
    }
    .encode_canonical()
    .map_err(|error| GenesisError::validation(format!("encode network binding: {error}")))?;
    Ok(Some(hex::encode(binding)))
}

fn compressed_reth_p2p_public(node_id: &str) -> Result<String, GenesisError> {
    let uncompressed = hex::decode(node_id)
        .map_err(|error| GenesisError::validation(format!("invalid Reth P2P node id: {error}")))?;
    if uncompressed.len() != 64 {
        return Err(GenesisError::validation(format!(
            "Reth P2P node id must be 64 bytes, got {}",
            uncompressed.len()
        )));
    }
    let prefix = if uncompressed[63] & 1 == 0 {
        0x02
    } else {
        0x03
    };
    let mut compressed = [0_u8; 33];
    compressed[0] = prefix;
    compressed[1..].copy_from_slice(&uncompressed[..32]);
    Ok(hex::encode(compressed))
}

pub(super) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), GenesisError> {
    fs::create_dir_all(dst).map_err(|err| GenesisError::io(dst, err))?;
    for entry_res in WalkDir::new(src) {
        let entry = entry_res.map_err(|e| GenesisError::io(src, std::io::Error::other(e)))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .map_err(|e| GenesisError::io(src, std::io::Error::other(e)))?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).map_err(|err| GenesisError::io(&target, err))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| GenesisError::io(parent, err))?;
            }
            fs::copy(entry.path(), &target).map_err(|err| GenesisError::io(&target, err))?;
        }
    }
    Ok(())
}

/// Copy the complete signed SGX release runtime into the bundle and strip signing keys.
pub fn stage_signed_enclave(
    config: &Value,
    output_dir: &Path,
) -> Result<Option<PathBuf>, GenesisError> {
    let Some(source) = config.get("signed_enclave_dir").and_then(Value::as_str) else {
        return Ok(None);
    };
    if source.is_empty() {
        return Ok(None);
    }
    let source_dir = Path::new(source);
    let nested_install_root = source_dir.join("rootfs/opt/outbe/sgx");
    let source_dir = if nested_install_root.is_dir() {
        nested_install_root.as_path()
    } else {
        source_dir
    };
    for &name in SIGNED_ENCLAVE_FILES {
        let origin = source_dir.join(name);
        if !origin.is_file() {
            return Err(GenesisError::validation(format!(
                "`signed_enclave_dir` is missing {name}. Point it at either an \
                 extracted signed release bundle or its rootfs/opt/outbe/sgx directory."
            )));
        }
    }

    let staged = output_dir.join("sgx");
    if staged.exists() {
        fs::remove_dir_all(&staged).map_err(|err| GenesisError::io(&staged, err))?;
    }
    copy_dir_recursive(source_dir, &staged)?;

    let entries = WalkDir::new(&staged)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| GenesisError::io(&staged, std::io::Error::other(err)))?;
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("pem") {
            fs::remove_file(path).map_err(|err| GenesisError::io(path, err))?;
        }
    }
    Ok(Some(staged))
}

fn write_validator_scripts(
    config: &Value,
    index: usize,
    validator: &Value,
    paths: &BundlePaths<'_>,
    direct_dev_binding: Option<&str>,
    direct_dev_node_id: Option<&str>,
) -> Result<(), GenesisError> {
    let directory = paths.output_dir.join(format!("validator-{index}"));
    fs::create_dir_all(&directory).map_err(|err| GenesisError::io(&directory, err))?;
    let p2p_address = validator
        .get("p2p_address")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("validator missing p2p_address"))?;
    let host = p2p_address
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(p2p_address);

    write_script(
        &directory.join("run-enclave.sh"),
        &enclave_script(
            config,
            index,
            paths.base_dir,
            direct_dev_binding,
            direct_dev_node_id,
        )?,
    )?;
    write_script(
        &directory.join("run-radicle.sh"),
        &radicle_script(config, index, host, paths.remote_keys_dir)?,
    )?;

    let mut node_config = config.clone();
    if let Some(obj) = node_config.as_object_mut() {
        obj.insert(
            "remote_keys_dir".to_owned(),
            Value::String(paths.remote_keys_dir.to_owned()),
        );
    }
    write_script(
        &directory.join("run-node.sh"),
        &node_script(&node_config, index, paths.base_dir, p2p_address)?,
    )?;
    write_script(
        &directory.join("run-feeder.sh"),
        &feeder_script(config, index, paths.base_dir),
    )?;
    let caddyfile_path = directory.join("Caddyfile");
    fs::write(&caddyfile_path, caddyfile(config, host)?)
        .map_err(|err| GenesisError::io(&caddyfile_path, err))?;
    write_script(
        &directory.join("run-tee-renew.sh"),
        &tee_renew_script(config, index, paths.base_dir, paths.remote_keys_dir)?,
    )?;
    write_script(
        &directory.join("preflight.sh"),
        &preflight_script(config, paths.base_dir, index)?,
    )?;
    write_script(
        &directory.join("install-caddy.sh"),
        &caddy_install_script(config, paths.base_dir, index)?,
    )?;

    let evm_key_path = paths
        .keys_dir
        .join(format!("validator-{index}"))
        .join("evm-key.hex");
    let raw_key =
        fs::read_to_string(&evm_key_path).map_err(|err| GenesisError::io(&evm_key_path, err))?;
    let signer_key = raw_key
        .trim()
        .strip_prefix("0x")
        .unwrap_or_else(|| raw_key.trim());
    let feeder_path = directory.join("feeder.toml");
    fs::write(
        &feeder_path,
        feeder_config(config, index, validator, signer_key)?,
    )
    .map_err(|err| GenesisError::io(&feeder_path, err))?;
    fs::set_permissions(&feeder_path, fs::Permissions::from_mode(0o600))
        .map_err(|err| GenesisError::io(&feeder_path, err))?;
    let pairs_path = directory.join("pairs.toml");
    fs::write(&pairs_path, feeder_pairs(config)?)
        .map_err(|err| GenesisError::io(&pairs_path, err))?;

    let tee_port = port_of(config, "tee_enclave_port")?;
    let start_all = start_all_script(config, index, paths.base_dir)?
        .replace("{TEE_PORT}", &tee_port.to_string());
    write_script(&directory.join("start-all.sh"), &start_all)?;
    write_script(
        &directory.join("stop-all.sh"),
        &stop_all_script(index, paths.base_dir),
    )?;
    Ok(())
}

/// Render all scripts, systemd units, distribution archives, and `DEPLOY.md` for `validators`.
pub fn render(
    config: &Value,
    validators: &[Value],
    genesis_path: &Path,
    keys_dir: &Path,
) -> Result<(), GenesisError> {
    let output_dir = genesis_path.parent().unwrap_or_else(|| Path::new("."));
    let base_dir = config
        .get("remote_base_dir")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| output_dir.display().to_string());
    let remote_keys_dir = config
        .get("remote_keys_dir")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| keys_dir.display().to_string());

    let reth_p2p_port = port_of(config, "reth_p2p_port")?;
    let mut enodes = Vec::with_capacity(validators.len());
    let mut reth_p2p_public_keys = Vec::with_capacity(validators.len());
    for (index, validator) in validators.iter().enumerate() {
        let node_id = ensure_reth_p2p_secret(&keys_dir.join(format!("validator-{index}")))?;
        reth_p2p_public_keys.push(compressed_reth_p2p_public(&node_id)?);
        let p2p_address = validator
            .get("p2p_address")
            .and_then(Value::as_str)
            .ok_or_else(|| GenesisError::validation("validator missing p2p_address"))?;
        let host = p2p_address
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(p2p_address);
        enodes.push(format!(
            "enode://{node_id}@{}:{reth_p2p_port}",
            format_enode_host(host)
        ));
    }
    let bootnodes_path = output_dir.join("reth-bootnodes.txt");
    fs::write(&bootnodes_path, format!("{}\n", enodes.join("\n")))
        .map_err(|err| GenesisError::io(&bootnodes_path, err))?;

    let paths = BundlePaths {
        output_dir,
        keys_dir,
        base_dir: &base_dir,
        remote_keys_dir: &remote_keys_dir,
    };
    let direct_dev_binding = direct_dev_network_binding(config, genesis_path)?;
    for (index, validator) in validators.iter().enumerate() {
        write_validator_scripts(
            config,
            index,
            validator,
            &paths,
            direct_dev_binding.as_deref(),
            direct_dev_binding
                .as_ref()
                .map(|_| reth_p2p_public_keys[index].as_str()),
        )?;
    }

    stage_signed_enclave(config, output_dir)?;
    write_systemd_units(output_dir, &base_dir, uses_production_tee_session(config))?;
    build_distribution(output_dir, validators, keys_dir, &base_dir)?;

    let mut deploy_config = config.clone();
    if let Some(obj) = deploy_config.as_object_mut() {
        obj.insert(
            "remote_base_dir".to_owned(),
            Value::String(base_dir.clone()),
        );
    }
    let deploy_path = output_dir.join("DEPLOY.md");
    fs::write(
        &deploy_path,
        deploy_markdown(&deploy_config, validators, keys_dir, &enodes)?,
    )
    .map_err(|err| GenesisError::io(&deploy_path, err))?;
    Ok(())
}
