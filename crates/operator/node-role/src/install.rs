use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::{
        fs::{symlink, MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use eyre::{bail, ensure, Context, Result};
use walkdir::WalkDir;

use crate::{
    assets, config::NodeRole, disk_tree::child::terminate_process_group, path_text, process,
};

pub(crate) const REQUIRED_SECRETS: &[&str] = &[
    "signing-key.hex",
    "evm-key.hex",
    "feeder-key.hex",
    "hyperlane-key.hex",
    "reth-p2p-secret.hex",
    "radicle/keys/radicle",
];

pub(crate) const CANONICAL_HEX_KEYS: &[&str] = &[
    "evm-key.hex",
    "feeder-key.hex",
    "hyperlane-key.hex",
    "reth-p2p-secret.hex",
];

const ENV_KEYS: &[&str] = &[
    "OUTBE_NODE_ROLE",
    "OUTBE_EXTERNAL_IP",
    "OUTBE_RPC_BIND_IP",
    "OUTBE_RPC_PORT",
    "OUTBE_VM_IP",
    "OUTBE_VM_PREFIX",
    "OUTBE_VM_GATEWAY",
    "OUTBE_VM_DNS",
    "OUTBE_RETH_P2P_PORT",
    "OUTBE_RETH_DISCOVERY_PORT",
    "OUTBE_CONSENSUS_PORT",
    "OUTBE_RADICLE_PORT",
    "OUTBE_CERTIFIED_RPC",
    "OUTBE_VALIDATOR_ADDRESS",
    "OUTBE_BOOTNODES",
    "OUTBE_CONSENSUS_PEERS",
    "OUTBE_CONSENSUS_EXTRA_ARGS",
    "OUTBE_HYPERLANE_FUNDING_WEI",
    "OUTBE_CHAIN_ID",
];

const FULL_NODE_ENV_KEYS: &[&str] = &[
    "OUTBE_NODE_ROLE",
    "OUTBE_EXTERNAL_IP",
    "OUTBE_RPC_BIND_IP",
    "OUTBE_RPC_PORT",
    "OUTBE_VM_IP",
    "OUTBE_VM_PREFIX",
    "OUTBE_VM_GATEWAY",
    "OUTBE_VM_DNS",
    "OUTBE_RETH_P2P_PORT",
    "OUTBE_RETH_DISCOVERY_PORT",
    "OUTBE_CONSENSUS_PORT",
    "OUTBE_RADICLE_PORT",
    "OUTBE_CERTIFIED_RPC",
    "OUTBE_BOOTNODES",
    "OUTBE_CONSENSUS_EXTRA_ARGS",
    "OUTBE_CHAIN_ID",
];

const VM_STATIC_NET_KEYS: &[&str] = &[
    "OUTBE_VM_IP",
    "OUTBE_VM_PREFIX",
    "OUTBE_VM_GATEWAY",
    "OUTBE_VM_DNS",
];

pub fn run() -> Result<()> {
    process::require_root()?;
    let root = Path::new(process::ROOT);
    let env_path = root.join("validator.env");
    let feeder = root.join("feeder-public.toml");
    let hyperlane = root.join("hyperlane-public.toml");
    let reth = root.join("reth.toml");
    validate_local_config(&env_path)?;
    validate_local_config(&feeder)?;
    validate_local_config(&hyperlane)?;
    validate_local_config(&reth)?;
    let environment = read_environment(&env_path)?;
    validate_environment(&environment)?;
    validate_bundle(root)?;

    install_binary(&root.join("outbe-up"), Path::new("/usr/local/bin/outbe-up"))?;
    validate_genesis(root, &environment["OUTBE_CHAIN_ID"])?;
    let chain_id: u64 = environment["OUTBE_CHAIN_ID"]
        .parse()
        .wrap_err("OUTBE_CHAIN_ID must be a positive integer")?;
    let hyperlane_public =
        fs::read_to_string(&hyperlane).wrap_err("failed to read hyperlane-public.toml")?;
    crate::config::validate_hyperlane_public_toml(&hyperlane_public, chain_id, false)?;
    let reth_config = fs::read_to_string(&reth).wrap_err("failed to read reth.toml")?;
    crate::config::validate_reth_toml(&reth_config)?;
    ensure_outbe_account()?;
    let uid = numeric_id("-u")?;
    let gid = numeric_id("-g")?;
    install_state(root, uid, gid)?;

    process::chown(&env_path, 0, gid)?;
    process::chown(&feeder, 0, gid)?;
    process::chown(&hyperlane, 0, gid)?;
    process::chown(&reth, 0, gid)?;
    fs::set_permissions(&env_path, fs::Permissions::from_mode(0o640))?;
    fs::set_permissions(&feeder, fs::Permissions::from_mode(0o640))?;
    fs::set_permissions(&hyperlane, fs::Permissions::from_mode(0o640))?;
    fs::set_permissions(&reth, fs::Permissions::from_mode(0o640))?;
    install_units(root)?;
    process::systemctl(&["daemon-reload"])?;
    Ok(())
}

fn validate_local_config(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("missing local configuration: {}", path.display()))?;
    ensure!(
        metadata.is_file(),
        "configuration is not a file: {}",
        path.display()
    );
    ensure!(
        !metadata.file_type().is_symlink(),
        "configuration is a symlink: {}",
        path.display()
    );
    ensure!(
        metadata.uid() == 0,
        "configuration must be root-owned: {}",
        path.display()
    );
    ensure!(
        metadata.mode() & 0o022 == 0,
        "configuration must not be group/world writable: {}",
        path.display()
    );
    Ok(())
}

pub(crate) fn read_environment(path: &Path) -> Result<BTreeMap<String, String>> {
    read_environment_with_keys(path, ENV_KEYS, false)
}

pub(crate) fn read_environment_for_role(
    path: &Path,
    role: NodeRole,
) -> Result<BTreeMap<String, String>> {
    match role {
        NodeRole::Founder | NodeRole::JoiningValidator => {
            read_environment_with_keys(path, ENV_KEYS, true)
        }
        NodeRole::FullNode => read_environment_with_keys(path, FULL_NODE_ENV_KEYS, true),
    }
}

fn read_environment_with_keys(
    path: &Path,
    allowed_keys: &[&str],
    require_vm_static_net: bool,
) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    let allowed = allowed_keys.iter().copied().collect::<BTreeSet<_>>();
    for line in fs::read_to_string(path)?.lines() {
        let (name, value) = parse_env_line(line, &allowed)?;
        ensure!(
            values.insert(name.to_owned(), value.to_owned()).is_none(),
            "duplicate validator.env key: {name}"
        );
    }
    for name in allowed_keys
        .iter()
        .copied()
        .filter(|name| env_key_required(name, require_vm_static_net))
    {
        ensure!(
            values.get(name).is_some_and(|value| !value.is_empty()),
            "missing {name} in validator.env"
        );
    }
    Ok(values)
}

/// Split one `NAME="value"` line of validator.env into an allowed key and its unquoted value.
fn parse_env_line<'a>(line: &'a str, allowed: &BTreeSet<&str>) -> Result<(&'a str, &'a str)> {
    let (name, quoted) = line
        .split_once('=')
        .ok_or_else(|| eyre::eyre!("invalid validator.env line: {line}"))?;
    ensure!(allowed.contains(name), "unknown validator.env key: {name}");
    ensure!(
        quoted.len() >= 2 && quoted.starts_with('"') && quoted.ends_with('"'),
        "invalid validator.env line: {line}"
    );
    let value = &quoted[1..quoted.len() - 1];
    ensure!(
        !value.contains(['"', '\n', '\r']),
        "invalid validator.env value: {name}"
    );
    Ok((name, value))
}

/// Returns true when validator.env must set `name` to a non-empty value. The VM-only keys are
/// optional for a bare-host validator.env.
fn env_key_required(name: &str, require_vm_static_net: bool) -> bool {
    name != "OUTBE_CONSENSUS_EXTRA_ARGS" && (require_vm_static_net || !is_vm_only_env_key(name))
}

fn is_vm_only_env_key(name: &str) -> bool {
    name == "OUTBE_NODE_ROLE"
        || name == "OUTBE_RPC_PORT"
        || name == "OUTBE_HYPERLANE_FUNDING_WEI"
        || VM_STATIC_NET_KEYS.contains(&name)
}

pub(crate) fn validate_environment(values: &BTreeMap<String, String>) -> Result<()> {
    validate_environment_inner(values, NodeRole::JoiningValidator, false)
}

pub(crate) fn validate_environment_for_role(
    values: &BTreeMap<String, String>,
    role: NodeRole,
) -> Result<()> {
    validate_environment_inner(values, role, true)
}

fn validate_environment_inner(
    values: &BTreeMap<String, String>,
    role: NodeRole,
    require_vm_static_net: bool,
) -> Result<()> {
    if let Some(raw_role) = values.get("OUTBE_NODE_ROLE") {
        let parsed: NodeRole = raw_role.parse().wrap_err("invalid OUTBE_NODE_ROLE")?;
        if require_vm_static_net {
            ensure!(
                parsed == role,
                "OUTBE_NODE_ROLE ({parsed}) does not match expected role ({role})"
            );
        }
    } else if require_vm_static_net {
        eyre::bail!("missing OUTBE_NODE_ROLE in validator.env");
    }
    values["OUTBE_EXTERNAL_IP"]
        .parse::<std::net::Ipv4Addr>()
        .wrap_err("OUTBE_EXTERNAL_IP must be an IPv4 literal")?;
    let rpc_bind = values["OUTBE_RPC_BIND_IP"]
        .parse::<std::net::Ipv4Addr>()
        .wrap_err("OUTBE_RPC_BIND_IP must be an IPv4 literal")?;
    ensure!(
        rpc_bind.is_loopback() || rpc_bind.is_private(),
        "OUTBE_RPC_BIND_IP must be a loopback or private IPv4 address"
    );
    let any_vm_net = VM_STATIC_NET_KEYS.iter().any(|k| values.contains_key(*k));
    if require_vm_static_net || any_vm_net {
        let static_net = crate::vm_network::StaticVmNetworkConfig::from_env(values)?;
        ensure!(
            rpc_bind == std::net::Ipv4Addr::LOCALHOST || rpc_bind == static_net.address,
            "OUTBE_RPC_BIND_IP ({rpc_bind}) must be 127.0.0.1 or equal to OUTBE_VM_IP ({}) for VM roles",
            static_net.address
        );
        let _ = crate::vm_network::VmPortsConfig::from_env(values)?;
    } else {
        let _ = crate::vm_network::VmPortsConfig::from_env_bare_host(values)?;
    }
    let rpc = &values["OUTBE_CERTIFIED_RPC"];
    ensure!(
        (rpc.starts_with("http://") || rpc.starts_with("https://"))
            && !rpc.contains(char::is_whitespace),
        "OUTBE_CERTIFIED_RPC must be an HTTP(S) URL"
    );
    ensure!(
        values["OUTBE_CHAIN_ID"]
            .parse::<u64>()
            .is_ok_and(|id| id > 0),
        "OUTBE_CHAIN_ID must be positive"
    );
    if let Some(raw) = values.get("OUTBE_HYPERLANE_FUNDING_WEI") {
        ensure!(
            raw.parse::<alloy_primitives::U256>()
                .is_ok_and(|amount| !amount.is_zero()),
            "OUTBE_HYPERLANE_FUNDING_WEI must be a positive integer"
        );
    }
    ensure!(
        values["OUTBE_BOOTNODES"].starts_with("enode://"),
        "OUTBE_BOOTNODES is invalid"
    );
    let extra = values
        .get("OUTBE_CONSENSUS_EXTRA_ARGS")
        .map_or("", String::as_str);
    ensure!(
        extra.is_empty() || extra == "--consensus.use-local-defaults",
        "OUTBE_CONSENSUS_EXTRA_ARGS is invalid"
    );
    if matches!(role, NodeRole::Founder | NodeRole::JoiningValidator) {
        ensure!(
            valid_address(&values["OUTBE_VALIDATOR_ADDRESS"]),
            "OUTBE_VALIDATOR_ADDRESS must be a 20-byte hex address"
        );
        let peers = values["OUTBE_CONSENSUS_PEERS"]
            .split(',')
            .collect::<Vec<_>>();
        ensure!(
            !peers.is_empty() && peers.iter().all(|peer| valid_peer(peer)),
            "OUTBE_CONSENSUS_PEERS is invalid"
        );
    }
    Ok(())
}

fn validate_bundle(root: &Path) -> Result<()> {
    required_file(&root.join("genesis.json"))?;
    required_file(&root.join(".bb-crs/bn254_g1.dat"))?;
    for name in [
        "outbe-up",
        "outbe-chain",
        "outbe-cli",
        "outbe-keygen",
        "outbe-feeder",
        "outbe-hyperlane-validator",
        "outbe-radicle",
    ] {
        required_executable(&root.join(name))?;
    }
    Ok(())
}

pub(crate) fn validate_genesis(root: &Path, expected: &str) -> Result<()> {
    let genesis: serde_json::Value = serde_json::from_slice(&fs::read(root.join("genesis.json"))?)?;
    let actual = genesis
        .pointer("/config/chainId")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| eyre::eyre!("genesis.json has no numeric config.chainId"))?;
    ensure!(
        actual.to_string() == expected,
        "genesis chain ID does not match validator.env"
    );
    Ok(())
}

pub(crate) fn ensure_outbe_account() -> Result<()> {
    if !process::command_succeeds("getent", &["group", "outbe"]) {
        process::run("groupadd", &["--system", "outbe"])?;
    }
    if !process::command_succeeds("getent", &["passwd", "outbe"]) {
        process::run(
            "useradd",
            &[
                "--system",
                "--gid",
                "outbe",
                "--home-dir",
                process::ROOT,
                "--shell",
                "/usr/sbin/nologin",
                "outbe",
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn numeric_id(flag: &str) -> Result<u32> {
    process::capture("id", &[flag, "outbe"])?
        .trim()
        .parse()
        .wrap_err("invalid outbe account id")
}

fn install_state(root: &Path, uid: u32, gid: u32) -> Result<()> {
    let keys = root.join("keys");
    let validator = root.join("validator-0");
    let state = root.join("state");
    let operator_state = state.join("operator");
    let feeder_state = state.join("feeder");
    let hyperlane_state = state.join("hyperlane");
    for path in [
        &keys,
        &validator,
        &state,
        &operator_state,
        &feeder_state,
        &hyperlane_state,
    ] {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            ensure!(
                !metadata.file_type().is_symlink(),
                "refusing symlink state path: {}",
                path.display()
            );
        }
    }
    ensure!(
        keys.is_dir(),
        "keys directory is missing: {}",
        keys.display()
    );
    for relative in [
        "validator-0",
        "validator-0/data",
        "validator-0/consensus",
        "validator-0/logs",
        "keys/radicle/storage",
        "keys/radicle/node",
        "keys/radicle/cobs",
    ] {
        create_owned_dir(&root.join(relative), 0o750, uid, gid)?;
    }
    create_owned_dir(&state, 0o750, 0, gid)?;
    create_owned_dir(&operator_state, 0o700, 0, 0)?;
    create_owned_dir(&feeder_state, 0o700, uid, gid)?;
    create_owned_dir(&hyperlane_state, 0o700, uid, gid)?;
    for relative in REQUIRED_SECRETS {
        let path = keys.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .wrap_err_with(|| format!("required secret is missing: {relative}"))?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "required secret is invalid: {relative}"
        );
    }
    for entry in WalkDir::new(&keys).follow_links(false) {
        let entry = entry?;
        ensure!(
            !entry.file_type().is_symlink(),
            "refusing symlink in keys directory: {}",
            entry.path().display()
        );
        process::chown(entry.path(), uid, gid)?;
        if entry.file_type().is_dir() {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700))?;
        }
    }
    for relative in REQUIRED_SECRETS {
        fs::set_permissions(keys.join(relative), fs::Permissions::from_mode(0o600))?;
    }
    for relative in CANONICAL_HEX_KEYS {
        canonical_key(&keys.join(relative))?;
    }
    verify_signing_key(root, &keys)?;
    Ok(())
}

pub(crate) fn verify_signing_key(bin_dir: &Path, keys_dir: &Path) -> Result<()> {
    let never = AtomicBool::new(false);
    verify_signing_key_cancellable(bin_dir, keys_dir, &never)
}

pub(crate) fn verify_signing_key_cancellable(
    bin_dir: &Path,
    keys_dir: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "outbe-keygen verify canceled by signal"
    );
    let keygen = bin_dir.join("outbe-keygen");
    ensure!(
        keygen.is_file(),
        "outbe-keygen binary is missing: {}",
        keygen.display()
    );
    let mut cmd = Command::new(&keygen);
    cmd.args([
        "verify",
        "--key",
        path_text(&keys_dir.join("signing-key.hex"))?,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .process_group(0);

    let mut child = cmd
        .spawn()
        .wrap_err_with(|| format!("failed to execute {}", keygen.display()))?;
    let status = wait_keygen_verify(&mut child, &keygen, cancel)?;
    ensure!(
        status.success(),
        "signing-key.hex failed outbe-keygen verify (exit status: {status})"
    );
    Ok(())
}

/// Poll `outbe-keygen verify` until it exits. On cancel, stop its process group and reap it.
fn wait_keygen_verify(child: &mut Child, keygen: &Path, cancel: &AtomicBool) -> Result<ExitStatus> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            terminate_process_group(child.id());
            let _ = child.wait();
            bail!("outbe-keygen verify canceled by signal");
        }
        if let Some(status) = child
            .try_wait()
            .wrap_err_with(|| format!("wait for {} verify", keygen.display()))?
        {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn install_units(root: &Path) -> Result<()> {
    let unit_dir = root.join("systemd");
    if let Ok(metadata) = fs::symlink_metadata(&unit_dir) {
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "refusing invalid systemd directory: {}",
            unit_dir.display()
        );
    }
    create_owned_dir(&unit_dir, 0o755, 0, 0)?;
    for (name, content) in assets::systemd_units()? {
        let canonical = unit_dir.join(&name);
        process::write_file(&canonical, content.as_bytes(), 0o644)?;
        process::chown(&canonical, 0, 0)?;

        let systemd_link = Path::new("/etc/systemd/system").join(name);
        if let Ok(metadata) = fs::symlink_metadata(&systemd_link) {
            ensure!(
                !metadata.is_dir(),
                "refusing systemd unit directory: {}",
                systemd_link.display()
            );
            fs::remove_file(&systemd_link)?;
        }
        symlink(&canonical, &systemd_link)?;
    }
    Ok(())
}

fn install_binary(source: &Path, destination: &Path) -> Result<()> {
    install_file(source, destination, 0o755)
}

fn install_file(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o755))?;
    }
    let file_name = destination
        .file_name()
        .ok_or_else(|| eyre::eyre!("install destination has no file name"))?;
    let temporary = destination.with_file_name(format!(
        ".{}.tmp.{}",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    fs::copy(source, &temporary)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    process::chown(&temporary, 0, 0)?;
    fs::rename(temporary, destination)?;
    Ok(())
}

pub(crate) fn create_owned_dir(path: &Path, mode: u32, uid: u32, gid: u32) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    process::chown(path, uid, gid)
}

pub(crate) fn required_file(path: &Path) -> Result<()> {
    ensure!(path.is_file(), "missing bundle file: {}", path.display());
    Ok(())
}

fn required_executable(path: &Path) -> Result<()> {
    let metadata =
        fs::metadata(path).wrap_err_with(|| format!("missing executable: {}", path.display()))?;
    ensure!(
        metadata.is_file() && metadata.mode() & 0o111 != 0,
        "missing executable: {}",
        path.display()
    );
    Ok(())
}

pub(crate) fn canonical_key(path: &Path) -> Result<()> {
    let value = fs::read_to_string(path)?;
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{} is not canonical lowercase hex",
        path.display()
    );
    Ok(())
}

fn valid_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_peer(value: &str) -> bool {
    let Some((key, address)) = value.split_once('@') else {
        return false;
    };
    let Some((host, port)) = address.rsplit_once(':') else {
        return false;
    };
    key.len() == 96
        && key.bytes().all(|byte| byte.is_ascii_hexdigit())
        && host.parse::<std::net::Ipv4Addr>().is_ok()
        && port.parse::<u16>().is_ok_and(|port| port > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_requires_the_packaged_crs() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("genesis.json"), b"{}").unwrap();
        for name in [
            "outbe-up",
            "outbe-chain",
            "outbe-cli",
            "outbe-keygen",
            "outbe-feeder",
            "outbe-hyperlane-validator",
            "outbe-radicle",
        ] {
            let path = root.path().join(name);
            fs::write(&path, b"binary").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(validate_bundle(root.path())
            .unwrap_err()
            .to_string()
            .contains(".bb-crs/bn254_g1.dat"));

        let crs = root.path().join(".bb-crs/bn254_g1.dat");
        fs::create_dir_all(crs.parent().unwrap()).unwrap();
        fs::write(crs, b"CRS").unwrap();
        validate_bundle(root.path()).unwrap();
    }

    #[test]
    fn bare_host_env_without_rpc_port_remains_valid_while_vm_env_requires_it() {
        let root = tempfile::tempdir().unwrap();
        let peer_key = "a".repeat(96);
        let bare_env = format!(
            concat!(
                "OUTBE_EXTERNAL_IP=\"192.0.2.10\"\n",
                "OUTBE_RPC_BIND_IP=\"127.0.0.1\"\n",
                "OUTBE_RETH_P2P_PORT=\"30303\"\n",
                "OUTBE_RETH_DISCOVERY_PORT=\"31303\"\n",
                "OUTBE_CONSENSUS_PORT=\"30400\"\n",
                "OUTBE_RADICLE_PORT=\"8776\"\n",
                "OUTBE_CERTIFIED_RPC=\"https://rpc.example\"\n",
                "OUTBE_VALIDATOR_ADDRESS=\"0x1111111111111111111111111111111111111111\"\n",
                "OUTBE_BOOTNODES=\"enode://abcd@192.0.2.1:30303\"\n",
                "OUTBE_CONSENSUS_PEERS=\"{peer_key}@192.0.2.1:30400\"\n",
                "OUTBE_CONSENSUS_EXTRA_ARGS=\"\"\n",
                "OUTBE_CHAIN_ID=\"54322345\"\n",
            ),
            peer_key = peer_key
        );
        let env_path = root.path().join("validator.env");
        fs::write(&env_path, &bare_env).unwrap();
        let parsed = read_environment(&env_path).unwrap();
        validate_environment(&parsed).unwrap();

        let mut invalid_funding = parsed.clone();
        for (name, value) in [
            ("OUTBE_NODE_ROLE", "founder"),
            ("OUTBE_RPC_PORT", "8545"),
            ("OUTBE_VM_IP", "10.77.0.10"),
            ("OUTBE_VM_PREFIX", "24"),
            ("OUTBE_VM_GATEWAY", "10.77.0.1"),
            ("OUTBE_VM_DNS", "1.1.1.1"),
            ("OUTBE_HYPERLANE_FUNDING_WEI", "0"),
        ] {
            invalid_funding.insert(name.to_owned(), value.to_owned());
        }
        let funding_err = validate_environment_for_role(&invalid_funding, NodeRole::Founder)
            .unwrap_err()
            .to_string();
        assert!(funding_err.contains("positive integer"), "{funding_err}");

        // VM env without OUTBE_RPC_PORT must still fail closed.
        let vm_env = format!(
            concat!(
                "OUTBE_NODE_ROLE=\"founder\"\n",
                "OUTBE_VM_IP=\"10.77.0.10\"\n",
                "OUTBE_VM_PREFIX=\"24\"\n",
                "OUTBE_VM_GATEWAY=\"10.77.0.1\"\n",
                "OUTBE_VM_DNS=\"1.1.1.1\"\n",
                "{bare_env}",
            ),
            bare_env = bare_env
        );
        fs::write(&env_path, &vm_env).unwrap();
        assert!(read_environment_for_role(&env_path, NodeRole::Founder)
            .unwrap_err()
            .to_string()
            .contains("OUTBE_RPC_PORT"));
    }
}
