use std::{
    env, fs,
    os::unix::{fs::FileTypeExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};

use crate::{process, rpc::RpcClient};

const RADICLE_SOCKET: &str = "/opt/outbe-chain/keys/radicle/node/outbe-control.sock";

pub fn feeder(bin_dir: Option<&Path>) -> Result<()> {
    let chain_id = required_env("OUTBE_CHAIN_ID")?;
    ensure!(
        chain_id.parse::<u64>().is_ok_and(|value| value > 0),
        "OUTBE_CHAIN_ID must be a positive decimal integer"
    );
    let validator = required_env("OUTBE_VALIDATOR_ADDRESS")?;
    ensure!(valid_address(&validator), "invalid OUTBE_VALIDATOR_ADDRESS");
    let rpc = local_rpc_url()?;

    let key_path = Path::new(process::ROOT).join("keys/feeder-key.hex");
    let key = fs::read_to_string(&key_path)
        .wrap_err_with(|| format!("cannot read feeder key: {}", key_path.display()))?;
    ensure!(valid_key(&key), "feeder key is not canonical");
    let public = fs::read_to_string(Path::new(process::ROOT).join("feeder-public.toml"))
        .wrap_err("cannot read public feeder configuration")?;

    let runtime = env::var_os("RUNTIME_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/outbe-feeder"));
    fs::create_dir_all(&runtime)?;
    let config = runtime.join("feeder.toml");
    let is_vm_role = env::var_os("OUTBE_NODE_ROLE").is_some_and(|v| !v.is_empty());
    let health_bind = feeder_health_bind_address(is_vm_role);
    let mut content = format!(
        concat!(
            "[chain]\n",
            "rpc_endpoint = {rpc}\n",
            "chain_id = {chain_id}\n",
            "gasless_oracle_votes = true\n\n",
            "[account]\n",
            "private_key = {private_key}\n",
            "validator_address = {validator}\n\n",
            "[health]\n",
            "enabled = true\n",
            "bind_address = {health_bind}\n\n"
        ),
        chain_id = chain_id,
        rpc = serde_json::to_string(&rpc)?,
        private_key = serde_json::to_string(&format!("0x{key}"))?,
        validator = serde_json::to_string(&validator)?,
        health_bind = serde_json::to_string(health_bind)?,
    );
    content.push_str(&public);
    if !content.ends_with('\n') {
        content.push('\n');
    }
    process::write_file(&config, content.as_bytes(), 0o600)?;
    let mut command = Command::new(binary_path(bin_dir, "outbe-feeder"));
    command.args(["--config", &config.to_string_lossy()]);
    Err(command.exec()).wrap_err("failed to exec outbe-feeder")
}

pub(crate) fn hyperlane(bin_dir: Option<&Path>) -> Result<()> {
    let chain_id = required_env("OUTBE_CHAIN_ID")?;
    ensure!(
        chain_id.parse::<u64>().is_ok_and(|value| value > 0),
        "OUTBE_CHAIN_ID must be a positive decimal integer"
    );
    let validator = required_env("OUTBE_VALIDATOR_ADDRESS")?;
    ensure!(valid_address(&validator), "invalid OUTBE_VALIDATOR_ADDRESS");
    let rpc = local_rpc_url()?;
    let root = Path::new(process::ROOT);
    let key_path = env::var_os("OUTBE_HYPERLANE_KEY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("keys/hyperlane-key.hex"));
    let key = fs::read_to_string(&key_path)
        .wrap_err_with(|| format!("cannot read Hyperlane key: {}", key_path.display()))?;
    ensure!(valid_key(&key), "Hyperlane key is not canonical");
    let public_path = env::var_os("OUTBE_HYPERLANE_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("hyperlane-public.toml"));
    let public = fs::read_to_string(&public_path).wrap_err_with(|| {
        format!(
            "cannot read public Hyperlane configuration: {}",
            public_path.display()
        )
    })?;

    let runtime = env::var_os("RUNTIME_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/outbe-hyperlane-agent"));
    fs::create_dir_all(&runtime)?;
    let config = runtime.join("hyperlane-agent.toml");
    let mut content = format!(
        concat!(
            "[chain]\n",
            "rpc_endpoint = {rpc}\n",
            "chain_id = {chain_id}\n\n",
            "[account]\n",
            "private_key = {private_key}\n",
            "validator_address = {validator}\n\n",
        ),
        rpc = serde_json::to_string(&rpc)?,
        chain_id = chain_id,
        private_key = serde_json::to_string(&format!("0x{}", key.trim()))?,
        validator = serde_json::to_string(&validator)?,
    );
    content.push_str(&public);
    if !content.ends_with('\n') {
        content.push('\n');
    }
    process::write_file(&config, content.as_bytes(), 0o600)?;
    let mut command = Command::new(binary_path(bin_dir, "outbe-hyperlane-agent"));
    command.args(["--config", &config.to_string_lossy()]);
    Err(command.exec()).wrap_err("failed to exec outbe-hyperlane-agent")
}

fn binary_path(bin_dir: Option<&Path>, name: &str) -> PathBuf {
    bin_dir
        .unwrap_or_else(|| Path::new(process::ROOT))
        .join(name)
}

fn feeder_health_bind_address(is_vm_role: bool) -> &'static str {
    if is_vm_role {
        "0.0.0.0:9002"
    } else {
        "127.0.0.1:9002"
    }
}

pub async fn wait_validator_dependencies(timeout: u64) -> Result<()> {
    wait_for("Radicle control socket", timeout, None, || async {
        let metadata = fs::metadata(RADICLE_SOCKET).wrap_err("Radicle control socket is absent")?;
        ensure!(
            metadata.file_type().is_socket(),
            "Radicle control path is not a socket"
        );
        Ok(())
    })
    .await
}

pub async fn wait_rpc(timeout: u64) -> Result<()> {
    let expected = required_env("OUTBE_CHAIN_ID")?
        .parse::<u64>()
        .wrap_err("OUTBE_CHAIN_ID must be a positive decimal integer")?;
    let rpc = RpcClient::new(local_rpc_url()?)?;
    let main_pid = env::var("MAINPID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|pid| *pid > 0);
    wait_for("local validator RPC", timeout, main_pid, || async {
        let actual = rpc.chain_id().await?;
        ensure!(
            actual == expected,
            "local RPC chain id {actual} does not match {expected}"
        );
        Ok(())
    })
    .await
}

async fn wait_for<F, Fut>(
    description: &str,
    timeout: u64,
    watched_pid: Option<u32>,
    mut probe: F,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let deadline = (timeout > 0).then(|| Instant::now() + Duration::from_secs(timeout));
    loop {
        let last_error = match tokio::time::timeout(Duration::from_secs(2), probe()).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) => error.to_string(),
            Err(_) => "probe timed out".to_owned(),
        };
        if watched_pid.is_some_and(|pid| !process_is_live(Path::new("/proc"), pid)) {
            bail!(
                "outbe-chain exited before {description} became ready: {last_error}; inspect the service journal"
            );
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("timed out waiting for {description}: {last_error}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn process_is_live(proc_root: &Path, pid: u32) -> bool {
    let Ok(status) = fs::read_to_string(proc_root.join(pid.to_string()).join("status")) else {
        return false;
    };
    !status.lines().any(|line| {
        line.strip_prefix("State:")
            .is_some_and(|state| state.trim_start().starts_with('Z'))
    })
}

fn required_env(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| eyre::eyre!("missing environment variable: {name}"))
}

pub(crate) fn local_rpc_url() -> Result<String> {
    let bind_raw = required_env("OUTBE_RPC_BIND_IP")?;
    let is_vm_role = env::var_os("OUTBE_NODE_ROLE").is_some_and(|v| !v.is_empty());
    let port_raw = env::var("OUTBE_RPC_PORT").ok().filter(|v| !v.is_empty());
    local_rpc_url_from(&bind_raw, port_raw.as_deref(), is_vm_role)
}

fn local_rpc_url_from(
    bind_raw: &str,
    port_raw: Option<&str>,
    require_port: bool,
) -> Result<String> {
    let bind = bind_raw
        .parse::<std::net::Ipv4Addr>()
        .wrap_err("OUTBE_RPC_BIND_IP must be an IPv4 literal")?;
    ensure!(
        bind.is_loopback() || bind.is_private(),
        "OUTBE_RPC_BIND_IP must be a loopback or private IPv4 address"
    );
    let port: u16 = match port_raw {
        Some(raw_port) => {
            ensure!(
                raw_port.bytes().all(|b| b.is_ascii_digit()),
                "OUTBE_RPC_PORT must be a decimal port in 1..=65535, got `{raw_port}`"
            );
            let parsed: u16 = raw_port.parse().wrap_err_with(|| {
                format!("OUTBE_RPC_PORT must be in 1..=65535, got `{raw_port}`")
            })?;
            ensure!(parsed > 0, "OUTBE_RPC_PORT must be in 1..=65535, got 0");
            parsed
        }
        None if require_port => {
            bail!("missing environment variable: OUTBE_RPC_PORT");
        }
        None => crate::vm_network::DEFAULT_RPC_PORT,
    };
    Ok(format!("http://{bind}:{port}"))
}

fn valid_key(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn zero_timeout_waits_without_a_deadline() {
        let mut attempts = 0;
        wait_for("test RPC", 0, None, || {
            attempts += 1;
            async move {
                if attempts < 2 {
                    Err(eyre::eyre!("RPC unavailable"))
                } else {
                    Ok(())
                }
            }
        })
        .await
        .expect("zero timeout must wait until the probe succeeds");
        assert_eq!(attempts, 2);
    }

    #[tokio::test]
    async fn wait_stops_when_watched_process_has_exited() {
        let error = wait_for("test RPC", 60, Some(u32::MAX), || async {
            Err(eyre::eyre!("RPC unavailable"))
        })
        .await
        .expect_err("missing process must stop the wait");
        assert!(error.to_string().contains("outbe-chain exited"));
    }

    #[test]
    fn watched_process_must_exist_and_not_be_a_zombie() {
        let proc_root = tempfile::tempdir().unwrap();
        let live = proc_root.path().join("11");
        let zombie = proc_root.path().join("12");
        fs::create_dir_all(&live).unwrap();
        fs::create_dir_all(&zombie).unwrap();
        fs::write(
            live.join("status"),
            "Name:\toutbe-chain\nState:\tS (sleeping)\n",
        )
        .unwrap();
        fs::write(
            zombie.join("status"),
            "Name:\toutbe-chain\nState:\tZ (zombie)\n",
        )
        .unwrap();

        assert!(process_is_live(proc_root.path(), 11));
        assert!(!process_is_live(proc_root.path(), 12));
        assert!(!process_is_live(proc_root.path(), 13));
    }

    #[test]
    fn binary_path_defaults_to_root_and_honors_override() {
        assert_eq!(
            binary_path(None, "outbe-feeder"),
            PathBuf::from("/opt/outbe-chain/outbe-feeder")
        );
        assert_eq!(
            binary_path(Some(Path::new("/usr/local/bin")), "outbe-feeder"),
            PathBuf::from("/usr/local/bin/outbe-feeder")
        );
        assert_eq!(
            binary_path(Some(Path::new("/usr/local/bin")), "outbe-hyperlane-agent"),
            PathBuf::from("/usr/local/bin/outbe-hyperlane-agent")
        );
    }

    #[test]
    fn local_rpc_url_defaults_to_8545_on_bare_host_and_requires_port_on_vm() {
        assert_eq!(
            local_rpc_url_from("127.0.0.1", None, false).unwrap(),
            "http://127.0.0.1:8545"
        );
        assert_eq!(
            local_rpc_url_from("127.0.0.1", Some("18545"), false).unwrap(),
            "http://127.0.0.1:18545"
        );
        assert_eq!(
            local_rpc_url_from("10.77.0.10", Some("18545"), true).unwrap(),
            "http://10.77.0.10:18545"
        );
        assert!(local_rpc_url_from("10.77.0.10", None, true)
            .unwrap_err()
            .to_string()
            .contains("OUTBE_RPC_PORT"));
        assert!(local_rpc_url_from("127.0.0.1", Some("0"), false).is_err());
        assert!(local_rpc_url_from("127.0.0.1", Some("abc"), false).is_err());
    }

    #[test]
    fn feeder_health_binds_all_interfaces_in_vm_and_loopback_on_bare_host() {
        assert_eq!(feeder_health_bind_address(true), "0.0.0.0:9002");
        assert_eq!(feeder_health_bind_address(false), "127.0.0.1:9002");
    }
}
