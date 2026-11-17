//! Render the per-machine launch bundle that goes with a generated genesis.

pub mod dist;
pub mod scripts;
pub mod systemd;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::{env, fs};

use serde_json::Value;
use walkdir::WalkDir;

use crate::error::GenesisError;
pub use crate::keys::ensure_reth_p2p_secret;
use crate::network::packaged_crs_path;
use crate::seed_codec::parse_u64;

pub use self::dist::{build_distribution, deploy_markdown};
pub use self::scripts::{
    feeder_config, feeder_pairs, feeder_pairs_file, feeder_pairs_settings, feeder_pairs_table,
    feeder_script, node_script, radicle_script,
};
pub use self::systemd::{caddy_install_script, caddyfile, preflight_script, write_systemd_units};

/// Default port numbers shared across validator hosts.
pub const DEFAULT_PORTS: &[(&str, u16)] = &[
    ("reth_p2p_port", 30303),
    ("reth_discv5_port", 31303),
    ("rpc_port", 8545),
    ("authrpc_port", 8551),
    ("metrics_port", 9101),
    ("radicle_port", 8776),
    ("radicle_status_port", 8876),
    ("feeder_health_port", 9002),
    ("public_rpc_port", 80),
    ("public_radicle_status_port", 8080),
];

/// Protocol ceiling on the validator set, mirroring the ValidatorSet precompile default.
pub const DEFAULT_MAX_VALIDATORS: u64 = 128;

/// Run one command with root privileges without requiring `sudo` when the
/// generated installer is already running as root.
pub(super) const ROOT_HELPER: &str = r#"as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo -- "$@"
  else
    echo "root privileges required: run as root or install sudo" >&2
    return 1
  fi
}"#;

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

fn stage_crs(config: &Value, output_dir: &Path) -> Result<(), GenesisError> {
    let source = config
        .get("crs_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(packaged_crs_path)
        .or_else(|| env::var_os("BB_CRS_PATH").map(PathBuf::from))
        .ok_or_else(|| {
            GenesisError::validation("CRS is missing; use the packaged release or set `crs_path`")
        })?;
    if !source.is_file() {
        return Err(GenesisError::validation(format!(
            "CRS file does not exist: {}",
            source.display()
        )));
    }
    let destination = output_dir.join(".bb-crs/bn254_g1.dat");
    fs::create_dir_all(destination.parent().expect("CRS path has a parent"))
        .map_err(|err| GenesisError::io(&destination, err))?;
    fs::copy(&source, &destination).map_err(|err| GenesisError::io(&destination, err))?;
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o644))
        .map_err(|err| GenesisError::io(&destination, err))
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
        &directory.join("preflight.sh"),
        &preflight_script(config, paths.base_dir, index)?,
    )?;
    write_script(
        &directory.join("install-caddy.sh"),
        &caddy_install_script(config, paths.base_dir, index)?,
    )?;

    let feeder_key_path = paths
        .keys_dir
        .join(format!("validator-{index}"))
        .join("feeder-key.hex");
    let raw_key = fs::read_to_string(&feeder_key_path)
        .map_err(|err| GenesisError::io(&feeder_key_path, err))?;
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

    write_systemd_units(&directory, paths.base_dir, index)?;
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

    stage_crs(config, output_dir)?;
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
