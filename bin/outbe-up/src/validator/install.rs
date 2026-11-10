use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use eyre::{bail, ensure, Context, Result};
use walkdir::WalkDir;

use crate::{
    support::process,
    validator::{assets, path_text},
};

const ENV_KEYS: &[&str] = &[
    "OUTBE_EXTERNAL_IP",
    "OUTBE_CERTIFIED_RPC",
    "OUTBE_VALIDATOR_ADDRESS",
    "OUTBE_BOOTNODES",
    "OUTBE_CONSENSUS_PEERS",
    "OUTBE_CONSENSUS_EXTRA_ARGS",
    "OUTBE_ENCLAVE_RUNTIME",
    "OUTBE_CHAIN_ID",
];

pub fn run() -> Result<()> {
    process::require_root()?;
    let root = Path::new(process::ROOT);
    let env_path = root.join("validator.env");
    let feeder = root.join("feeder-public.toml");
    validate_local_config(&env_path)?;
    validate_local_config(&feeder)?;
    let environment = read_environment(&env_path)?;
    validate_environment(&environment)?;
    validate_runtime(root, &environment)?;
    validate_bundle(root)?;

    if environment["OUTBE_ENCLAVE_RUNTIME"] == "system-gramine" {
        install_system_gramine_paths(root)?;
    }
    install_binary(&root.join("outbe-up"), Path::new("/usr/local/bin/outbe-up"))?;
    validate_genesis(root, &environment["OUTBE_CHAIN_ID"])?;
    ensure_outbe_account()?;
    let uid = numeric_id("-u")?;
    let gid = numeric_id("-g")?;
    install_state(root, uid, gid)?;

    process::chown(&env_path, 0, gid)?;
    process::chown(&feeder, 0, gid)?;
    fs::set_permissions(&env_path, fs::Permissions::from_mode(0o640))?;
    fs::set_permissions(&feeder, fs::Permissions::from_mode(0o640))?;
    install_units()?;
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

fn read_environment(path: &Path) -> Result<BTreeMap<String, String>> {
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

fn validate_environment(values: &BTreeMap<String, String>) -> Result<()> {
    values["OUTBE_EXTERNAL_IP"]
        .parse::<std::net::Ipv4Addr>()
        .wrap_err("OUTBE_EXTERNAL_IP must be an IPv4 literal")?;
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

fn validate_runtime(root: &Path, environment: &BTreeMap<String, String>) -> Result<()> {
    match environment["OUTBE_ENCLAVE_RUNTIME"].as_str() {
        "system-gramine" => {
            ensure!(
                command_in_path("gramine-sgx"),
                "gramine-sgx is not installed"
            );
            required_file(&root.join("outbe-tee-enclave.manifest.sgx"))?;
            required_file(&root.join("outbe-tee-enclave.sig"))?;
        }
        "bundled-sgx" => {
            required_executable(&root.join("sgx/bin/outbe-tee-enclave-launch"))?;
            for relative in [
                "bin/outbe-tee-enclave",
                "gramine/loader",
                "gramine/libpal.so",
                "network-descriptor-v1.bin",
                "outbe-tee-enclave.manifest.sgx",
                "outbe-tee-enclave.sig",
            ] {
                required_file(&root.join("sgx").join(relative))?;
            }
        }
        _ => {
            bail!("OUTBE_ENCLAVE_RUNTIME must be system-gramine or bundled-sgx");
        }
    }
    Ok(())
}

fn validate_bundle(root: &Path) -> Result<()> {
    for name in ["genesis.json", "SHA256SUMS"] {
        required_file(&root.join(name))?;
    }
    for name in [
        "outbe-up",
        "outbe-chain",
        "outbe-cli",
        "outbe-keygen",
        "outbe-feeder",
        "outbe-radicle",
        "outbe-tee-enclave",
    ] {
        required_executable(&root.join(name))?;
    }
    process::capture_in("sha256sum", &["-c", "--quiet", "SHA256SUMS"], root)
        .wrap_err("bundle checksum verification failed")?;
    Ok(())
}

fn validate_genesis(root: &Path, expected: &str) -> Result<()> {
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

fn ensure_outbe_account() -> Result<()> {
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

fn numeric_id(flag: &str) -> Result<u32> {
    process::capture("id", &[flag, "outbe"])?
        .trim()
        .parse()
        .wrap_err("invalid outbe account id")
}

fn install_state(root: &Path, uid: u32, gid: u32) -> Result<()> {
    let keys = root.join("keys");
    let validator = root.join("validator-0");
    let tee = root.join("tee");
    for path in [&keys, &validator, &tee] {
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
    create_owned_dir(&tee, 0o700, 0, 0)?;
    let secrets = [
        "signing-key.hex",
        "evm-key.hex",
        "reth-p2p-secret.hex",
        "radicle/keys/radicle",
    ];
    for relative in secrets {
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
    for relative in secrets {
        fs::set_permissions(keys.join(relative), fs::Permissions::from_mode(0o600))?;
    }
    canonical_key(&keys.join("evm-key.hex"))?;
    canonical_key(&keys.join("reth-p2p-secret.hex"))?;
    process::capture(
        root.join("outbe-keygen"),
        &["verify", "--key", path_text(&keys.join("signing-key.hex"))?],
    )?;
    Ok(())
}

fn install_units() -> Result<()> {
    for (name, content) in assets::systemd_units()? {
        let path = Path::new("/etc/systemd/system").join(name);
        process::write_file(&path, content.as_bytes(), 0o644)?;
        process::chown(&path, 0, 0)?;
    }
    Ok(())
}

fn install_system_gramine_paths(root: &Path) -> Result<()> {
    let manifest = fs::read_to_string(root.join("outbe-tee-enclave.manifest.sgx"))?;
    let enclave = unique_manifest_path(&manifest, "/bin/outbe-tee-enclave")?;
    let descriptor = unique_manifest_path(&manifest, "/sgx/network-descriptor-v1.bin")?;
    install_binary(&root.join("outbe-tee-enclave"), &enclave)?;
    install_file(&root.join("network-descriptor-v1.bin"), &descriptor, 0o644)?;
    Ok(())
}

fn unique_manifest_path(manifest: &str, suffix: &str) -> Result<PathBuf> {
    let prefix = "/opt/outbe-chain/releases/";
    let mut matches = BTreeSet::new();
    for (start, _) in manifest.match_indices(prefix) {
        let tail = &manifest[start..];
        let end = tail
            .find(|character: char| {
                !(character.is_ascii_alphanumeric() || "/._-".contains(character))
            })
            .unwrap_or(tail.len());
        let value = &tail[..end];
        if value.ends_with(suffix) {
            matches.insert(PathBuf::from(value));
        }
    }
    ensure!(
        matches.len() == 1,
        "signed Gramine manifest must reference one {suffix} path"
    );
    Ok(matches.into_iter().next().expect("one manifest path"))
}

fn install_binary(source: &Path, destination: &Path) -> Result<()> {
    install_file(source, destination, 0o755)
}

fn install_file(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o755))?;
    }
    fs::copy(source, destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    process::chown(destination, 0, 0)
}

fn create_owned_dir(path: &Path, mode: u32, uid: u32, gid: u32) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    process::chown(path, uid, gid)
}

fn required_file(path: &Path) -> Result<()> {
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

fn canonical_key(path: &Path) -> Result<()> {
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

fn command_in_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| {
            fs::metadata(directory.join(name))
                .is_ok_and(|metadata| metadata.is_file() && metadata.mode() & 0o111 != 0)
        })
    })
}
