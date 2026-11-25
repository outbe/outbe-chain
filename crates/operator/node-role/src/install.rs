use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::Path,
};

use eyre::{ensure, Context, Result};
use walkdir::WalkDir;

use crate::{assets, path_text, process};

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
    "OUTBE_EXTERNAL_IP",
    "OUTBE_RPC_BIND_IP",
    "OUTBE_CERTIFIED_RPC",
    "OUTBE_VALIDATOR_ADDRESS",
    "OUTBE_BOOTNODES",
    "OUTBE_CONSENSUS_PEERS",
    "OUTBE_CONSENSUS_EXTRA_ARGS",
    "OUTBE_CHAIN_ID",
];

pub fn run() -> Result<()> {
    process::require_root()?;
    let root = Path::new(process::ROOT);
    let env_path = root.join("validator.env");
    let feeder = root.join("feeder-public.toml");
    let hyperlane = root.join("hyperlane-public.toml");
    validate_local_config(&env_path)?;
    validate_local_config(&feeder)?;
    validate_local_config(&hyperlane)?;
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
    ensure_outbe_account()?;
    let uid = numeric_id("-u")?;
    let gid = numeric_id("-g")?;
    install_state(root, uid, gid)?;

    process::chown(&env_path, 0, gid)?;
    process::chown(&feeder, 0, gid)?;
    process::chown(&hyperlane, 0, gid)?;
    fs::set_permissions(&env_path, fs::Permissions::from_mode(0o640))?;
    fs::set_permissions(&feeder, fs::Permissions::from_mode(0o640))?;
    fs::set_permissions(&hyperlane, fs::Permissions::from_mode(0o640))?;
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
    let mut values = BTreeMap::new();
    let allowed = ENV_KEYS.iter().copied().collect::<BTreeSet<_>>();
    for line in fs::read_to_string(path)?.lines() {
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
        ensure!(
            values.insert(name.to_owned(), value.to_owned()).is_none(),
            "duplicate validator.env key: {name}"
        );
    }
    for name in ENV_KEYS
        .iter()
        .copied()
        .filter(|name| !matches!(*name, "OUTBE_CONSENSUS_EXTRA_ARGS"))
    {
        ensure!(
            values.get(name).is_some_and(|value| !value.is_empty()),
            "missing {name} in validator.env"
        );
    }
    Ok(values)
}

pub(crate) fn validate_environment(values: &BTreeMap<String, String>) -> Result<()> {
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
    let rpc = &values["OUTBE_CERTIFIED_RPC"];
    ensure!(
        (rpc.starts_with("http://") || rpc.starts_with("https://"))
            && !rpc.contains(char::is_whitespace),
        "OUTBE_CERTIFIED_RPC must be an HTTP(S) URL"
    );
    ensure!(
        valid_address(&values["OUTBE_VALIDATOR_ADDRESS"]),
        "OUTBE_VALIDATOR_ADDRESS must be a 20-byte hex address"
    );
    ensure!(
        values["OUTBE_CHAIN_ID"]
            .parse::<u64>()
            .is_ok_and(|id| id > 0),
        "OUTBE_CHAIN_ID must be positive"
    );
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
    let peers = values["OUTBE_CONSENSUS_PEERS"]
        .split(',')
        .collect::<Vec<_>>();
    ensure!(
        !peers.is_empty() && peers.iter().all(|peer| valid_peer(peer)),
        "OUTBE_CONSENSUS_PEERS is invalid"
    );
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
    process::capture(
        bin_dir.join("outbe-keygen"),
        &[
            "verify",
            "--key",
            path_text(&keys_dir.join("signing-key.hex"))?,
        ],
    )?;
    Ok(())
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
}
