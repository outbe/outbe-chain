use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use alloy_primitives::U256;
use eyre::{ensure, Context, Result};

use super::{ensure_setup_via_rpc, required_env, save_state, RpcSetupRequest, SetupState};
use crate::{
    process::{self, HYPERLANE_STATE_ROOT},
    progress::Progress,
    rpc::RpcClient,
    validator_address, NodeRole,
};

/// Joining validators become active only after the background DKG activation.
/// Keep the service alive, register `H` at that boundary, then replace this
/// process with the actual daemon.
pub async fn run_service(bin_dir: Option<&Path>) -> Result<()> {
    if let Ok(role) = std::env::var("OUTBE_NODE_ROLE") {
        let role: NodeRole = role.parse()?;
        ensure!(
            matches!(role, NodeRole::Founder | NodeRole::JoiningValidator),
            "Hyperlane service is not valid for node role {role}"
        );
        return run_vm_service(bin_dir, role).await;
    }

    let wait_for_active = std::env::var_os("OUTBE_HYPERLANE_WAIT_FOR_ACTIVE").is_some();
    if !wait_for_active {
        return crate::service::hyperlane(bin_dir);
    }
    let rpc_url = crate::service::local_rpc_url()?;
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
        let (hash, submitted) = super::CliTransaction {
            key: &hyperlane_key,
            rpc_url: &rpc_url,
            args: &["validator", "set-hyperlane-signer", &format!("{signer:#x}")],
            drop_privileges: false,
            bin_dir,
        }
        .submit_once(state.signer_transaction.as_deref())?;
        if submitted {
            state.signer_transaction = Some(hash.clone());
            save_state(&state_path, &state)?;
        }
        crate::wait::wait_transaction_finalized(&rpc, &hash, &mut progress).await?;
        ensure!(
            rpc.hyperlane_signer(validator).await? == signer,
            "Hyperlane signer registration is not visible after finality"
        );
    }
    progress.done("Hyperlane signer is registered");
    crate::service::hyperlane(bin_dir)
}

/// VM validator roles have no guest SSH control path. Bring the complete Hyperlane signer
/// lifecycle on chain from the guest itself, journal every submitted transaction on the data
/// disk, and only then exec the long-running validator daemon.
async fn run_vm_service(bin_dir: Option<&Path>, role: NodeRole) -> Result<()> {
    let rpc_url = crate::service::local_rpc_url()?;
    let rpc = RpcClient::new(&rpc_url)?;
    let validator = required_env("OUTBE_VALIDATOR_ADDRESS")?.parse()?;
    let root = Path::new(process::ROOT);
    let validator_key = root.join("keys/evm-key.hex");
    let hyperlane_key = root.join("keys/hyperlane-key.hex");
    ensure!(
        validator_key.is_file(),
        "validator key does not name a file: {}",
        validator_key.display()
    );
    ensure!(
        hyperlane_key.is_file(),
        "Hyperlane key does not name a file: {}",
        hyperlane_key.display()
    );
    let funding = required_env("OUTBE_HYPERLANE_FUNDING_WEI")?
        .parse::<U256>()
        .wrap_err("OUTBE_HYPERLANE_FUNDING_WEI must be an integer")?;
    ensure!(
        !funding.is_zero(),
        "Hyperlane signer funding must be positive"
    );

    let mut progress = Progress::new();
    if role == NodeRole::JoiningValidator {
        loop {
            if rpc.validator(validator).await?.status == 2 {
                break;
            }
            progress.update("waiting for joining validator activation before Hyperlane setup");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    let state_path = std::env::var_os("STATE_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(HYPERLANE_STATE_ROOT))
        .join("setup.json");
    let state: SetupState = if state_path.is_file() {
        serde_json::from_slice(&fs::read(&state_path)?)?
    } else {
        SetupState::default()
    };
    let (state, complete) = ensure_setup_via_rpc(
        RpcSetupRequest {
            rpc: &rpc,
            validator_key: &validator_key,
            hyperlane_key: &hyperlane_key,
            funding,
        },
        state,
        None,
        &mut progress,
        |next| save_state(&state_path, next),
    )
    .await?;
    ensure!(complete, "Hyperlane setup stopped before completion");
    save_state(&state_path, &state)?;
    progress.done("Hyperlane signer is delegated, funded, and registered");
    crate::service::hyperlane(bin_dir)
}
