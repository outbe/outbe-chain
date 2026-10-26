//! Render the per-machine launch bundle that goes with a generated genesis.

pub mod dist;
pub mod scripts;
pub mod systemd;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

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

/// Files required in a signed Gramine SGX enclave directory.
pub const SIGNED_ENCLAVE_FILES: &[&str] = &[
    "outbe-tee-enclave",
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

/// Copy the signed enclave into the bundle and strip any `.pem` files.
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
    let staged = output_dir.join("enclave");
    fs::create_dir_all(&staged).map_err(|err| GenesisError::io(&staged, err))?;
    for &name in SIGNED_ENCLAVE_FILES {
        let origin = source_dir.join(name);
        if !origin.is_file() {
            return Err(GenesisError::validation(format!(
                "`signed_enclave_dir` is missing {name}. Sign the enclave on the \
                 build host first (gramine-manifest + gramine-sgx-sign) and point \
                 this at the directory holding the result."
            )));
        }
        let dst = staged.join(name);
        fs::copy(&origin, &dst).map_err(|err| GenesisError::io(&dst, err))?;
    }
    let dir_iter = fs::read_dir(&staged).map_err(|err| GenesisError::io(&staged, err))?;
    for entry_res in dir_iter {
        let entry = entry_res.map_err(|err| GenesisError::io(&staged, err))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("pem") {
            fs::remove_file(&path).map_err(|err| GenesisError::io(&path, err))?;
        }
    }
    Ok(Some(staged))
}

fn write_validator_scripts(
    config: &Value,
    index: usize,
    validator: &Value,
    paths: &BundlePaths<'_>,
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
        &enclave_script(config, index, paths.base_dir)?,
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
    for (index, validator) in validators.iter().enumerate() {
        let node_id = ensure_reth_p2p_secret(&keys_dir.join(format!("validator-{index}")))?;
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
    for (index, validator) in validators.iter().enumerate() {
        write_validator_scripts(config, index, validator, &paths)?;
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
