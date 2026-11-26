//! On-chain & `outbe-cli` driver for joining-validator funding, staking, delegation, and readiness.

use std::{
    fs,
    future::Future,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    pin::Pin,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use alloy_primitives::{Address, U256};
use eyre::{ensure, Context, Result};
use outbe_operator_node_role::rpc::{Block, RpcClient, ValidatorRecord};
use outbe_software_bundle::verify_and_extract;

/// Pluggable on-chain & `outbe-cli` driver for the joining-validator lifecycle.
///
/// Bare-host `crates/operator/node-role/src/promote.rs` cannot be reused directly here because it
/// executes on the validator host against `/opt/outbe-chain/keys` via `runuser --user outbe`; in
/// VM mode joiner keys stay strictly on the operator host (never placed on the VMM host) and
/// transactions are signed locally against `certified_rpc`. Tests inject a fake driver.
pub(crate) trait JoinChainDriver: Send + Sync {
    fn min_stake(&self) -> Pin<Box<dyn Future<Output = Result<U256>> + Send + '_>>;
    fn balance(&self, addr: Address) -> Pin<Box<dyn Future<Output = Result<U256>> + Send + '_>>;
    fn validator(
        &self,
        addr: Address,
    ) -> Pin<Box<dyn Future<Output = Result<ValidatorRecord>> + Send + '_>>;
    fn validator_delegate(
        &self,
        validator: Address,
        role: u8,
    ) -> Pin<Box<dyn Future<Output = Result<Address>> + Send + '_>>;
    fn finalized_block(&self) -> Pin<Box<dyn Future<Output = Result<Block>> + Send + '_>>;
    fn fund_address(
        &self,
        from_key: &Path,
        to: Address,
        amount: U256,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>>;
    fn stake(
        &self,
        joiner_keys_dir: &Path,
        validator: Address,
        amount: U256,
        symmetric_endpoint: &str,
    ) -> Result<String>;
    fn delegate_oracle(&self, joiner_evm_key: &Path, feeder: Address) -> Result<String>;
    fn confirm_ready(&self, joiner_evm_key: &Path) -> Result<String>;
    fn wait_tx_finalized(
        &self,
        hash: &str,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

pub(super) struct RealJoinChainDriver {
    rpc_url: String,
    rpc: RpcClient,
    cli_bin: PathBuf,
    _extract_dir: tempfile::TempDir,
}

impl RealJoinChainDriver {
    pub(super) fn from_artifacts(artifacts: &Path, rpc_url: &str) -> Result<Self> {
        let bundle = artifacts.join("software-bundle.tar.zst");
        let tmp = tempfile::Builder::new()
            .prefix("outbe-cli-join-")
            .tempdir()
            .wrap_err("create temp dir for bundled outbe-cli")?;
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700))?;
        verify_and_extract(&bundle, tmp.path())
            .wrap_err("verify software bundle for outbe-cli extraction")?;
        let cli_bin = tmp.path().join("bin/outbe-cli");
        ensure!(
            cli_bin.is_file(),
            "software bundle {} is missing bin/outbe-cli",
            bundle.display()
        );
        let rpc = RpcClient::new(rpc_url)?;
        Ok(Self {
            rpc_url: rpc_url.to_owned(),
            rpc,
            cli_bin,
            _extract_dir: tmp,
        })
    }

    fn run_cli(&self, key_file: &Path, args: &[&str]) -> Result<String> {
        let key_str = key_file
            .to_str()
            .ok_or_else(|| eyre::eyre!("non-UTF-8 key path: {}", key_file.display()))?;
        let output = Command::new(&self.cli_bin)
            .args(["--rpc-url", &self.rpc_url, "--private-key-file", key_str])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .wrap_err_with(|| format!("execute bundled {}", self.cli_bin.display()))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        ensure!(
            output.status.success(),
            "outbe-cli {} failed ({}): {}",
            args.join(" "),
            output.status,
            stderr.trim()
        );
        extract_tx_hash(&stdout).or_else(|_| extract_tx_hash(&stderr))
    }
}

impl JoinChainDriver for RealJoinChainDriver {
    fn min_stake(&self) -> Pin<Box<dyn Future<Output = Result<U256>> + Send + '_>> {
        Box::pin(async move { self.rpc.min_stake().await })
    }

    fn balance(&self, addr: Address) -> Pin<Box<dyn Future<Output = Result<U256>> + Send + '_>> {
        Box::pin(async move { self.rpc.balance(addr).await })
    }

    fn validator(
        &self,
        addr: Address,
    ) -> Pin<Box<dyn Future<Output = Result<ValidatorRecord>> + Send + '_>> {
        Box::pin(async move { self.rpc.validator(addr).await })
    }

    fn validator_delegate(
        &self,
        validator: Address,
        role: u8,
    ) -> Pin<Box<dyn Future<Output = Result<Address>> + Send + '_>> {
        Box::pin(async move { self.rpc.validator_delegate(validator, role).await })
    }

    fn finalized_block(&self) -> Pin<Box<dyn Future<Output = Result<Block>> + Send + '_>> {
        Box::pin(async move { self.rpc.finalized_block().await })
    }

    fn fund_address(
        &self,
        from_key: &Path,
        to: Address,
        amount: U256,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        let from_key = from_key.to_path_buf();
        Box::pin(async move {
            outbe_operator_node_role::hyperlane::send_value_transfer(
                &self.rpc, &from_key, to, amount,
            )
            .await
        })
    }

    fn stake(
        &self,
        joiner_keys_dir: &Path,
        validator: Address,
        amount: U256,
        symmetric_endpoint: &str,
    ) -> Result<String> {
        let evm_key = joiner_keys_dir.join("evm-key.hex");
        let keys_str = joiner_keys_dir
            .to_str()
            .ok_or_else(|| eyre::eyre!("non-UTF-8 keys dir: {}", joiner_keys_dir.display()))?;
        let val_arg = format!("{validator:#x}");
        let amt_arg = amount.to_string();
        self.run_cli(
            &evm_key,
            &[
                "staking",
                "stake",
                "--validator",
                &val_arg,
                "--amount",
                &amt_arg,
                "--keys-dir",
                keys_str,
                "--symmetric",
                symmetric_endpoint,
            ],
        )
    }

    fn delegate_oracle(&self, joiner_evm_key: &Path, feeder: Address) -> Result<String> {
        let feeder_arg = format!("{feeder:#x}");
        self.run_cli(
            joiner_evm_key,
            &["validator", "delegate", "oracle", &feeder_arg],
        )
    }

    fn confirm_ready(&self, joiner_evm_key: &Path) -> Result<String> {
        self.run_cli(joiner_evm_key, &["validator", "confirm-ready"])
    }

    fn wait_tx_finalized(
        &self,
        hash: &str,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let hash = hash.to_owned();
        Box::pin(async move {
            loop {
                if let Some(block) = self.rpc.transaction_block(&hash).await? {
                    let finalized = self.rpc.finalized_block().await?.number;
                    if finalized >= block {
                        return Ok(());
                    }
                }
                ensure!(
                    Instant::now() < deadline,
                    "timed out waiting for transaction {hash} to finalize"
                );
                let sleep_for = deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(3));
                tokio::time::sleep(sleep_for).await;
            }
        })
    }
}

fn extract_tx_hash(output: &str) -> Result<String> {
    output
        .split_whitespace()
        .find(|word| {
            word.len() == 66
                && word.starts_with("0x")
                && word[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .map(str::to_owned)
        .ok_or_else(|| eyre::eyre!("outbe-cli did not return a transaction hash: {output}"))
}
