use std::{
    env, fs,
    os::unix::{fs::FileTypeExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};

use crate::{support::process, validator::rpc::RpcClient};

const ENCLAVE_ADDRESS: &str = "127.0.0.1:17000";
const RADICLE_SOCKET: &str = "/opt/outbe-chain/keys/radicle/node/outbe-control.sock";
const LOCAL_RPC: &str = "http://127.0.0.1:8545";

pub fn enclave() -> Result<()> {
    let runtime = required_env("OUTBE_ENCLAVE_RUNTIME")?;
    let chain_id = required_env("OUTBE_CHAIN_ID")?
        .parse::<u64>()
        .wrap_err("OUTBE_CHAIN_ID must be a positive decimal integer")?;
    ensure!(chain_id > 0, "OUTBE_CHAIN_ID must be positive");
    let chain_hex = format!("0x{chain_id:064x}");
    let mut command = match runtime.as_str() {
        "system-gramine" => {
            let mut command = Command::new("gramine-sgx");
            command.arg("outbe-tee-enclave").current_dir(process::ROOT);
            command
        }
        "bundled-sgx" => {
            let sgx = Path::new(process::ROOT).join("sgx");
            let mut command = Command::new(sgx.join("bin/outbe-tee-enclave-launch"));
            command.current_dir(&sgx);
            command
        }
        _ => {
            bail!("OUTBE_ENCLAVE_RUNTIME must be system-gramine or bundled-sgx");
        }
    };
    let tee_dir = if runtime == "bundled-sgx" {
        "/var/lib/outbe/tee"
    } else {
        "/opt/outbe-chain/tee"
    };
    command.args([
        "--socket",
        ENCLAVE_ADDRESS,
        "--tee-dir",
        tee_dir,
        "--chain-id",
        &chain_hex,
    ]);
    Err(command.exec()).wrap_err("failed to exec enclave runtime")
}

pub fn feeder() -> Result<()> {
    let chain_id = required_env("OUTBE_CHAIN_ID")?;
    ensure!(
        chain_id.parse::<u64>().is_ok_and(|value| value > 0),
        "OUTBE_CHAIN_ID must be a positive decimal integer"
    );
    let validator = required_env("OUTBE_VALIDATOR_ADDRESS")?;
    ensure!(valid_address(&validator), "invalid OUTBE_VALIDATOR_ADDRESS");

    let key_path = Path::new(process::ROOT).join("keys/evm-key.hex");
    let key = fs::read_to_string(&key_path)
        .wrap_err_with(|| format!("cannot read Validator EVM key: {}", key_path.display()))?;
    ensure!(valid_key(&key), "Validator EVM key is not canonical");
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
            "rpc_endpoint = \"http://127.0.0.1:8545\"\n",
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

pub async fn wait_enclave(timeout: u64) -> Result<()> {
    wait_for("TEE enclave", timeout, || async {
        tokio::net::TcpStream::connect(ENCLAVE_ADDRESS)
            .await
            .map(|_| ())
            .wrap_err("enclave socket is not ready")
    })
    .await
}

pub async fn wait_validator_dependencies(timeout: u64) -> Result<()> {
    wait_for(
        "TEE enclave and Radicle control socket",
        timeout,
        || async {
            let metadata =
                fs::metadata(RADICLE_SOCKET).wrap_err("Radicle control socket is absent")?;
            ensure!(
                metadata.file_type().is_socket(),
                "Radicle control path is not a socket"
            );
            tokio::net::TcpStream::connect(ENCLAVE_ADDRESS)
                .await
                .map(|_| ())
                .wrap_err("enclave socket is not ready")
        },
    )
    .await
}

pub async fn wait_rpc(timeout: u64) -> Result<()> {
    let expected = required_env("OUTBE_CHAIN_ID")?
        .parse::<u64>()
        .wrap_err("OUTBE_CHAIN_ID must be a positive decimal integer")?;
    let rpc = RpcClient::new(LOCAL_RPC)?;
    wait_for("local validator RPC", timeout, || async {
        let actual = rpc.chain_id().await?;
        ensure!(
            actual == expected,
            "local RPC chain id {actual} does not match {expected}"
        );
        Ok(())
    })
    .await
}

async fn wait_for<F, Fut>(description: &str, timeout: u64, mut probe: F) -> Result<()>
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
