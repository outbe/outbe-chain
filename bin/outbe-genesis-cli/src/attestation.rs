//! Bind TEE policy to the seeded genesis via `outbe-chain tee genesis`.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::error::GenesisError;

/// Run `outbe-chain tee genesis` to bind the TEE policy to `seeded_genesis` and write `output`.
pub fn run_tee_stage(
    chain_binary: &str,
    seeded_genesis: &Path,
    output: &Path,
    config: &Value,
) -> Result<(), GenesisError> {
    let tee = config
        .get("tee")
        .and_then(Value::as_object)
        .ok_or_else(|| GenesisError::validation("missing `tee` object in config"))?;
    let mode = tee
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("missing `tee.mode` in config"))?;
    if mode != "no-attestation-dev" {
        return Err(GenesisError::validation(
            "only tee.mode no-attestation-dev is supported; TDX/SNP attestation is not implemented",
        ));
    }

    let args = vec![
        "tee".to_owned(),
        "genesis".to_owned(),
        "--input".to_owned(),
        seeded_genesis.to_string_lossy().into_owned(),
        "--output".to_owned(),
        output.to_string_lossy().into_owned(),
        "--mode".to_owned(),
        mode.to_owned(),
    ];

    let status = Command::new(chain_binary)
        .args(&args)
        .status()
        .map_err(|err| GenesisError::Subprocess {
            program: chain_binary.to_owned(),
            details: err.to_string(),
        })?;
    if !status.success() {
        return Err(GenesisError::Subprocess {
            program: chain_binary.to_owned(),
            details: format!("exited with {status}"),
        });
    }
    Ok(())
}
