use std::{
    env, fs,
    os::unix::{fs::FileTypeExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};

use crate::{support::process, validator::rpc::RpcClient};

const RADICLE_SOCKET: &str = "/opt/outbe-chain/keys/radicle/node/outbe-control.sock";

pub fn feeder() -> Result<()> {
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
            "bind_address = \"127.0.0.1:9002\"\n\n"
        ),
        chain_id = chain_id,
        rpc = serde_json::to_string(&rpc)?,
        private_key = serde_json::to_string(&format!("0x{key}"))?,
        validator = serde_json::to_string(&validator)?,
    );
    content.push_str(&public);
    if !content.ends_with('\n') {
        content.push('\n');
    }
    process::write_file(&config, content.as_bytes(), 0o600)?;
    let mut command = Command::new(Path::new(process::ROOT).join("outbe-feeder"));
    command.args(["--config", &config.to_string_lossy()]);
    Err(command.exec()).wrap_err("failed to exec outbe-feeder")
}

pub fn hyperlane() -> Result<()> {
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
    let mut command = Command::new(root.join("outbe-hyperlane-agent"));
    command.args(["--config", &config.to_string_lossy()]);
    Err(command.exec()).wrap_err("failed to exec outbe-hyperlane-agent")
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
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        let last_error = match tokio::time::timeout(Duration::from_secs(2), probe()).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) => error.to_string(),
            Err(_) => "probe timed out".to_owned(),
        };
        if watched_pid.is_some_and(|pid| !Path::new("/proc").join(pid.to_string()).exists()) {
            bail!(
                "outbe-chain exited before {description} became ready: {last_error}; inspect the service journal"
            );
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for {description}: {last_error}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn required_env(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| eyre::eyre!("missing environment variable: {name}"))
}

fn local_rpc_url() -> Result<String> {
    let bind = required_env("OUTBE_RPC_BIND_IP")?
        .parse::<std::net::Ipv4Addr>()
        .wrap_err("OUTBE_RPC_BIND_IP must be an IPv4 literal")?;
    ensure!(
        bind.is_loopback() || bind.is_private(),
        "OUTBE_RPC_BIND_IP must be a loopback or private IPv4 address"
    );
    Ok(format!("http://{bind}:8545"))
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
    async fn wait_stops_when_watched_process_has_exited() {
        let error = wait_for("test RPC", 60, Some(u32::MAX), || async {
            Err(eyre::eyre!("RPC unavailable"))
        })
        .await
        .expect_err("missing process must stop the wait");
        assert!(error.to_string().contains("outbe-chain exited"));
    }
}
