use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use alloy_primitives::U256;
use eyre::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    support::{
        process::{self, HYPERLANE_STATE_ROOT},
        progress::Progress,
    },
    validator::{path_text, rpc::RpcClient, validator_address, wait::wait_transaction_finalized},
};

const HYPERLANE_DELEGATE_ROLE: u8 = 2;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetupState {
    #[serde(default)]
    pub delegate_transaction: Option<String>,
    #[serde(default)]
    pub funding_transaction: Option<String>,
    #[serde(default)]
    pub signer_transaction: Option<String>,
}

pub struct SetupRequest<'a> {
    pub rpc_url: &'a str,
    pub validator_key: &'a Path,
    pub hyperlane_key: &'a Path,
    pub funding: U256,
}

pub async fn ensure_setup(
    request: SetupRequest<'_>,
    mut state: SetupState,
    progress: &mut Progress,
    mut save: impl FnMut(&SetupState) -> Result<()>,
) -> Result<SetupState> {
    let rpc = RpcClient::new(request.rpc_url)?;
    let validator = validator_address(request.validator_key)?;
    let hyperlane = validator_address(request.hyperlane_key)?;

    if rpc
        .validator_delegate(validator, HYPERLANE_DELEGATE_ROLE)
        .await?
        != hyperlane
    {
        progress.stage("delegating Hyperlane operations to the Hyperlane signer");
        let (hash, submitted) = transaction(
            state.delegate_transaction.as_deref(),
            request.validator_key,
            request.rpc_url,
            &[
                "validator",
                "delegate",
                "hyperlane",
                &format!("{hyperlane:#x}"),
            ],
            true,
        )?;
        if submitted {
            state.delegate_transaction = Some(hash.clone());
            save(&state)?;
        }
        wait_transaction_finalized(&rpc, &hash, progress).await?;
        ensure!(
            rpc.validator_delegate(validator, HYPERLANE_DELEGATE_ROLE)
                .await?
                == hyperlane,
            "Hyperlane delegation is not visible after finality"
        );
    }

    if rpc.balance(hyperlane).await?.is_zero() {
        progress.stage("funding the Hyperlane signer");
        let (hash, submitted) = transaction(
            state.funding_transaction.as_deref(),
            request.validator_key,
            request.rpc_url,
            &[
                "validator",
                "fund-operational-key",
                &format!("{hyperlane:#x}"),
                &request.funding.to_string(),
            ],
            true,
        )?;
        if submitted {
            state.funding_transaction = Some(hash.clone());
            save(&state)?;
        }
        wait_transaction_finalized(&rpc, &hash, progress).await?;
        ensure!(
            !rpc.balance(hyperlane).await?.is_zero(),
            "Hyperlane signer funding is not visible after finality"
        );
    }

    let record = rpc.validator(validator).await?;
    if record.status == 2 && rpc.hyperlane_signer(validator).await? != hyperlane {
        progress.stage("registering the Hyperlane checkpoint signer");
        let (hash, submitted) = transaction(
            state.signer_transaction.as_deref(),
            request.hyperlane_key,
            request.rpc_url,
            &[
                "validator",
                "set-hyperlane-signer",
                &format!("{hyperlane:#x}"),
            ],
            true,
        )?;
        if submitted {
            state.signer_transaction = Some(hash.clone());
            save(&state)?;
        }
        wait_transaction_finalized(&rpc, &hash, progress).await?;
        ensure!(
            rpc.hyperlane_signer(validator).await? == hyperlane,
            "Hyperlane signer registration is not visible after finality"
        );
    }
    Ok(state)
}

/// Joining validators become active only after the background DKG activation.
/// Keep the service alive, register `H` at that boundary, then replace this
/// process with the actual daemon.
pub async fn run_service() -> Result<()> {
    if std::env::var_os("OUTBE_HYPERLANE_WAIT_FOR_ACTIVE").is_none() {
        return super::service::hyperlane();
    }
    let rpc_bind = required_env("OUTBE_RPC_BIND_IP")?;
    let rpc_url = format!("http://{rpc_bind}:8545");
    let rpc = RpcClient::new(&rpc_url)?;
    let validator = required_env("OUTBE_VALIDATOR_ADDRESS")?.parse()?;
    let hyperlane_key = std::env::var_os("OUTBE_HYPERLANE_KEY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(process::ROOT).join("keys/hyperlane-key.hex"));
    ensure!(
        hyperlane_key.is_file(),
        "Hyperlane key does not name a file: {}",
        hyperlane_key.display()
    );
    let signer = validator_address(&hyperlane_key)?;
    let mut progress = Progress::new();

    loop {
        if rpc.validator(validator).await?.status == 2 {
            break;
        }
        progress.update("waiting for validator activation before Hyperlane registration");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    if rpc.hyperlane_signer(validator).await? != signer {
        progress.stage("registering the Hyperlane checkpoint signer");
        let state_path = std::env::var_os("STATE_DIRECTORY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(HYPERLANE_STATE_ROOT))
            .join("setup.json");
        let mut state: SetupState = if state_path.is_file() {
            serde_json::from_slice(&fs::read(&state_path)?)?
        } else {
            SetupState::default()
        };
        let (hash, submitted) = transaction(
            state.signer_transaction.as_deref(),
            &hyperlane_key,
            &rpc_url,
            &["validator", "set-hyperlane-signer", &format!("{signer:#x}")],
            false,
        )?;
        if submitted {
            state.signer_transaction = Some(hash.clone());
            save_state(&state_path, &state)?;
        }
        wait_transaction_finalized(&rpc, &hash, &mut progress).await?;
        ensure!(
            rpc.hyperlane_signer(validator).await? == signer,
            "Hyperlane signer registration is not visible after finality"
        );
    }
    progress.done("Hyperlane signer is registered");
    super::service::hyperlane()
}

pub async fn setup_founder() -> Result<()> {
    let rpc_bind = required_env("OUTBE_RPC_BIND_IP")?;
    let rpc_url = format!("http://{rpc_bind}:8545");
    let validator_key = required_path("OUTBE_VALIDATOR_KEY_PATH")?;
    let hyperlane_key = required_path("OUTBE_HYPERLANE_KEY_PATH")?;
    let funding = required_env("OUTBE_HYPERLANE_FUNDING_WEI")?
        .parse::<U256>()
        .wrap_err("OUTBE_HYPERLANE_FUNDING_WEI must be an integer")?;
    let state_path = Path::new(HYPERLANE_STATE_ROOT).join("setup.json");
    let state = if state_path.is_file() {
        serde_json::from_slice(&fs::read(&state_path)?)?
    } else {
        SetupState::default()
    };
    let mut progress = Progress::new();
    let state = ensure_setup(
        SetupRequest {
            rpc_url: &rpc_url,
            validator_key: &validator_key,
            hyperlane_key: &hyperlane_key,
            funding,
        },
        state,
        &mut progress,
        |state| save_state(&state_path, state),
    )
    .await?;
    save_state(&state_path, &state)?;
    progress.done("Hyperlane signer is delegated, funded, and registered");
    Ok(())
}

fn transaction(
    existing: Option<&str>,
    key: &Path,
    rpc_url: &str,
    args: &[&str],
    drop_privileges: bool,
) -> Result<(String, bool)> {
    if let Some(hash) = existing {
        return Ok((hash.to_owned(), false));
    }
    let binary = Path::new(process::ROOT).join("outbe-cli");
    let binary = path_text(&binary)?;
    let mut full = vec!["--rpc-url", rpc_url, "--private-key-file", path_text(key)?];
    full.extend_from_slice(args);
    let output = if drop_privileges {
        let mut runuser = vec!["--user", "outbe", "--", binary];
        runuser.extend(full);
        process::capture("runuser", &runuser)?
    } else {
        process::capture(binary, &full)?
    };
    Ok((transaction_hash(&output)?, true))
}

fn save_state(path: &Path, state: &SetupState) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state)?;
    process::write_file(path, &bytes, 0o600)
}

fn transaction_hash(output: &str) -> Result<String> {
    output
        .split_whitespace()
        .find(|word| {
            word.len() == 66
                && word.starts_with("0x")
                && word[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .map(str::to_owned)
        .ok_or_else(|| eyre::eyre!("command did not return a transaction hash: {output}"))
}

fn required_env(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| eyre::eyre!("missing environment variable: {name}"))
}

fn required_path(name: &str) -> Result<std::path::PathBuf> {
    let path = std::path::PathBuf::from(required_env(name)?);
    ensure!(path.is_file(), "{name} does not name a file");
    Ok(path)
}
