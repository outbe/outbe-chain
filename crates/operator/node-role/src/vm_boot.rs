//! Verify the unmeasured VM data disk and prepare runtime directories before daemons start.

mod fd_ops;
mod mount;
mod runtime_dirs;
#[cfg(test)]
mod tests;
mod tree_scan;

use std::{fs, io, os::unix::fs::MetadataExt, path::Path, sync::atomic::AtomicBool};

use eyre::{bail, ensure, Context, Result};

use self::{mount::verify_mount, runtime_dirs::prepare_directories, tree_scan::verify_tree};
use crate::{
    config::{NodeRole, NODE_ROLE_FILE},
    install, process, vm_network,
};

const DEFAULT_VM_BIN_DIR: &str = "/usr/local/bin";

/// Exact systemd service units started by `vm-boot` for each VM node role.
///
/// `outbe-hyperlane-agent.service` is intentionally excluded for all VM roles until S9
/// (Hyperlane delegation/funding/registration for VM networks).
pub fn vm_role_services(role: NodeRole) -> &'static [&'static str] {
    match role {
        NodeRole::Founder => &[
            "outbe-radicle.service",
            "outbe-validator.service",
            "outbe-feeder.service",
        ],
        NodeRole::JoiningValidator => &[
            "outbe-radicle.service",
            "outbe-joining-validator.service",
            "outbe-feeder.service",
        ],
        NodeRole::FullNode => &["outbe-full-node.service"],
    }
}

const CONFIG_FILES: &[&str] = &[
    "validator.env",
    "feeder-public.toml",
    "hyperlane-public.toml",
];

struct BootOptions<'a> {
    bin_dir: Option<&'a Path>,
    require_root_owner: bool,
    apply_ownership: bool,
    prepare_runtime_dirs: bool,
    configure_network: bool,
    start_role_services: bool,
    expected_role: Option<NodeRole>,
    cancel: Option<&'a AtomicBool>,
}

/// Read and strictly validate the VM node role from `/opt/outbe-chain/validator.env`
/// (`OUTBE_NODE_ROLE=founder|joining-validator|full-node`). If a legacy `/opt/outbe-chain/node-role`
/// file is also present on disk, both MUST agree or this fails closed.
pub fn read_role_from_tree(root: &Path, require_root_owner: bool) -> Result<NodeRole> {
    let env_path = root.join("validator.env");
    verify_config_file(&env_path, require_root_owner)?;
    let env_role = read_env_node_role(&env_path)?;

    let marker_path = root.join(NODE_ROLE_FILE);
    match fs::symlink_metadata(&marker_path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(env_role),
        Err(err) => Err(err).wrap_err_with(|| format!("failed to stat {}", marker_path.display())),
        Ok(_) => {
            verify_config_file(&marker_path, require_root_owner)?;
            let raw = fs::read_to_string(&marker_path)
                .wrap_err_with(|| format!("failed to read {}", marker_path.display()))?;
            let file_role = NodeRole::parse_marker(&raw)
                .wrap_err_with(|| format!("invalid role marker {}", marker_path.display()))?;
            ensure!(
                file_role == env_role,
                "role mismatch between validator.env OUTBE_NODE_ROLE ({env_role}) and {} ({file_role})",
                marker_path.display()
            );
            Ok(env_role)
        }
    }
}

fn read_env_node_role(env_path: &Path) -> Result<NodeRole> {
    let content = fs::read_to_string(env_path)
        .wrap_err_with(|| format!("failed to read {}", env_path.display()))?;
    let mut found: Option<NodeRole> = None;
    for line in content.lines() {
        let Some((key, quoted)) = line.split_once('=') else {
            bail!("invalid validator.env line: {line}");
        };
        if key != "OUTBE_NODE_ROLE" {
            continue;
        }
        ensure!(
            found.is_none(),
            "duplicate OUTBE_NODE_ROLE in {}",
            env_path.display()
        );
        ensure!(
            quoted.len() >= 2 && quoted.starts_with('"') && quoted.ends_with('"'),
            "invalid OUTBE_NODE_ROLE line in {}",
            env_path.display()
        );
        let value = &quoted[1..quoted.len() - 1];
        let parsed: NodeRole = value.parse().wrap_err_with(|| {
            format!(
                "invalid OUTBE_NODE_ROLE value in {} (must be founder, joining-validator, or full-node)",
                env_path.display()
            )
        })?;
        found = Some(parsed);
    }
    found.ok_or_else(|| {
        eyre::eyre!(
            "missing OUTBE_NODE_ROLE in {} (must be founder, joining-validator, or full-node)",
            env_path.display()
        )
    })
}

/// Verify an emitted VM data tree without checking live mountinfo or changing ownership.
pub fn verify_tree_without_mount(root: &Path, bin_dir: &Path) -> Result<()> {
    let mountinfo = format!(
        "100 1 8:1 / {} rw,nosuid,nodev,noexec - ext4 /dev/vda1 rw\n",
        root.display()
    );
    boot_root(
        root,
        &mountinfo,
        &BootOptions {
            bin_dir: Some(bin_dir),
            require_root_owner: false,
            apply_ownership: false,
            prepare_runtime_dirs: true,
            configure_network: false,
            start_role_services: false,
            expected_role: None,
            cancel: None,
        },
    )
}

/// Strictly verify a static VM data tree for `expected_role` without mutating `root` or checking
/// live `/proc/self/mountinfo`.
pub fn verify_static_tree_for_role(
    root: &Path,
    expected_role: NodeRole,
    bin_dir: Option<&Path>,
) -> Result<()> {
    let never = AtomicBool::new(false);
    verify_static_tree_for_role_cancellable(root, expected_role, bin_dir, &never)
}

/// Strictly verify a static VM data tree for `expected_role` with cooperative signal cancellation
/// forwarded into `outbe-keygen verify`.
pub fn verify_static_tree_for_role_cancellable(
    root: &Path,
    expected_role: NodeRole,
    bin_dir: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<()> {
    let mountinfo = format!(
        "100 1 8:1 / {} rw,nosuid,nodev,noexec - ext4 /dev/vda1 rw\n",
        root.display()
    );
    boot_root(
        root,
        &mountinfo,
        &BootOptions {
            bin_dir,
            require_root_owner: false,
            apply_ownership: false,
            prepare_runtime_dirs: false,
            configure_network: false,
            start_role_services: false,
            expected_role: Some(expected_role),
            cancel: Some(cancel),
        },
    )
}

/// Verify `/opt/outbe-chain` and prepare runtime directories during VM boot.
pub fn run(bin_dir: Option<&Path>) -> Result<()> {
    process::require_root()?;
    let root = Path::new(process::ROOT);
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .wrap_err("failed to read /proc/self/mountinfo")?;
    let bin_dir = bin_dir.unwrap_or_else(|| Path::new(DEFAULT_VM_BIN_DIR));
    boot_root(
        root,
        &mountinfo,
        &BootOptions {
            bin_dir: Some(bin_dir),
            require_root_owner: true,
            apply_ownership: true,
            prepare_runtime_dirs: true,
            configure_network: true,
            start_role_services: true,
            expected_role: None,
            cancel: None,
        },
    )
}

fn boot_root(root: &Path, mountinfo: &str, options: &BootOptions<'_>) -> Result<()> {
    verify_mount(root, mountinfo)?;
    verify_tree(root)?;

    let role = read_role_from_tree(root, options.require_root_owner)?;
    if let Some(expected) = options.expected_role {
        ensure!(
            role == expected,
            "VM data disk role mismatch: expected {expected}, found {role}"
        );
    }

    let env_path = root.join("validator.env");
    let environment = match role {
        NodeRole::Founder | NodeRole::JoiningValidator => {
            for name in CONFIG_FILES {
                verify_config_file(&root.join(name), options.require_root_owner)?;
            }

            let environment = install::read_environment_for_role(&env_path, role)?;
            install::validate_environment_for_role(&environment, role)?;

            install::required_file(&root.join("genesis.json"))?;
            install::validate_genesis(root, &environment["OUTBE_CHAIN_ID"])?;
            let chain_id: u64 = environment["OUTBE_CHAIN_ID"]
                .parse()
                .wrap_err("OUTBE_CHAIN_ID must be a positive integer")?;
            let hyperlane_public = fs::read_to_string(root.join("hyperlane-public.toml"))
                .wrap_err("failed to read hyperlane-public.toml")?;
            crate::config::validate_hyperlane_public_toml(&hyperlane_public, chain_id, true)?;
            install::required_file(&root.join(".bb-crs/bn254_g1.dat"))?;

            verify_secrets(root, options.bin_dir, options.cancel)?;
            environment
        }
        NodeRole::FullNode => {
            verify_config_file(&env_path, options.require_root_owner)?;
            for forbidden in ["feeder-public.toml", "hyperlane-public.toml", "keys"] {
                let path = root.join(forbidden);
                ensure!(
                    fs::symlink_metadata(&path).is_err(),
                    "full-node data disk must not contain validator artifact: {}",
                    path.display()
                );
            }

            let environment = install::read_environment_for_role(&env_path, role)?;
            install::validate_environment_for_role(&environment, role)?;

            install::required_file(&root.join("genesis.json"))?;
            install::validate_genesis(root, &environment["OUTBE_CHAIN_ID"])?;
            install::required_file(&root.join(".bb-crs/bn254_g1.dat"))?;
            environment
        }
    };

    // Decision 10 (revised): every VM role uses plain static IPv4 addressing and explicit
    // listen/advertise ports from validator.env.
    let static_net = vm_network::StaticVmNetworkConfig::from_env(&environment)?;
    let _ports = vm_network::VmPortsConfig::from_env(&environment)?;

    if options.prepare_runtime_dirs {
        prepare_directories(root, role, options.apply_ownership)?;
    }

    if options.configure_network {
        let applied = match vm_network::configure_static_network(&static_net) {
            Ok(applied) => applied,
            Err(err) => {
                eprintln!(
                    "outbe-vm-boot: static IPv4 configuration failed for {}/{}: {err:#}",
                    static_net.address, static_net.prefix_len
                );
                return Err(err).wrap_err_with(|| {
                    format!(
                        "failed to configure VM static IP {}/{} during vm-boot",
                        static_net.address, static_net.prefix_len
                    )
                });
            }
        };
        eprintln!(
            "outbe-vm-boot: configured static IP {}/{} on {} (gw={}, dns={}, dropin={})",
            applied.address,
            applied.prefix_len,
            applied.iface,
            applied.gateway,
            vm_network::format_dns_list(&applied.dns),
            applied.dropin_path.display()
        );
    }

    if options.start_role_services {
        let services = vm_role_services(role);
        let mut args = vec!["start", "--no-block"];
        args.extend_from_slice(services);
        process::systemctl(&args).wrap_err_with(|| {
            format!(
                "failed to start VM role `{role}` services ({})",
                services.join(", ")
            )
        })?;
        eprintln!(
            "outbe-vm-boot: started role `{role}` services: {}",
            services.join(", ")
        );
    }
    Ok(())
}

fn verify_config_file(path: &Path, require_root_owner: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("missing required configuration: {}", path.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "configuration is not a regular file: {}",
        path.display()
    );
    if require_root_owner {
        ensure!(
            metadata.uid() == 0,
            "configuration must be root-owned: {}",
            path.display()
        );
    }
    let mode = metadata.mode() & 0o7777;
    ensure!(
        mode & 0o022 == 0,
        "configuration must not be group/world writable (mode {mode:04o}): {}",
        path.display()
    );
    Ok(())
}

fn verify_secrets(root: &Path, bin_dir: Option<&Path>, cancel: Option<&AtomicBool>) -> Result<()> {
    let keys = root.join("keys");
    let keys_meta = fs::symlink_metadata(&keys)
        .wrap_err_with(|| format!("keys directory is missing: {}", keys.display()))?;
    ensure!(
        keys_meta.is_dir() && !keys_meta.file_type().is_symlink(),
        "keys path is not a directory: {}",
        keys.display()
    );
    for relative in install::REQUIRED_SECRETS {
        let path = keys.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .wrap_err_with(|| format!("required secret is missing: {}", path.display()))?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "required secret is not a regular file: {}",
            path.display()
        );
        let mode = metadata.mode() & 0o7777;
        ensure!(
            mode & 0o077 == 0,
            "required secret must have owner-only permissions (mode {mode:04o}): {}",
            path.display()
        );
    }
    for relative in install::CANONICAL_HEX_KEYS {
        install::canonical_key(&keys.join(relative))?;
    }
    if let Some(bin_dir) = bin_dir {
        match cancel {
            Some(flag) => install::verify_signing_key_cancellable(bin_dir, &keys, flag)?,
            None => install::verify_signing_key(bin_dir, &keys)?,
        }
    } else {
        let signing = fs::read_to_string(keys.join("signing-key.hex"))
            .wrap_err("failed to read signing-key.hex")?;
        let trimmed = signing.trim();
        ensure!(!trimmed.is_empty(), "signing-key.hex must not be empty");
    }
    Ok(())
}
