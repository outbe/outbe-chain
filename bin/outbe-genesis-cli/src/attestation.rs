//! Bind TEE policy to the seeded genesis via `outbe-chain tee genesis`.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::error::GenesisError;

const DCAP_OPTION_FLAGS: [(&str, &str); 5] = [
    ("--isv-prod-id", "isv_prod_id"),
    ("--minimum-isv-svn", "minimum_isv_svn"),
    (
        "--minimum-tcb-evaluation-data-number",
        "minimum_tcb_evaluation_data_number",
    ),
    ("--mrenclave", "mrenclave"),
    ("--mrsigner", "mrsigner"),
];

const SEV_SNP_OPTION_FLAGS: [(&str, &str); 3] = [
    ("--snp-ark-sha256", "snp_ark_sha256"),
    ("--snp-host-data", "snp_host_data"),
    ("--snp-measurement", "snp_measurement"),
];

const TDX_OPTION_FLAGS: [(&str, &str); 6] = [
    (
        "--tdx-minimum-tcb-evaluation-data-number",
        "tdx_minimum_tcb_evaluation_data_number",
    ),
    ("--tdx-mrconfigid", "tdx_mrconfigid"),
    ("--tdx-mrtd", "tdx_mrtd"),
    ("--tdx-rtmr0", "tdx_rtmr0"),
    ("--tdx-rtmr1", "tdx_rtmr1"),
    ("--tdx-rtmr2", "tdx_rtmr2"),
];

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

    let mut dcap_options: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for (flag, key) in DCAP_OPTION_FLAGS {
        let val = tee.get(key).and_then(|v| match v {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        });
        dcap_options.insert(flag, val);
    }

    let mut sev_snp_options: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for (flag, key) in SEV_SNP_OPTION_FLAGS {
        let val = tee.get(key).and_then(Value::as_str).map(str::to_owned);
        sev_snp_options.insert(flag, val);
    }
    // The mixed mode takes the identity of both platforms.
    let takes_sev_snp = mode == "sev-snp-required" || mode == "confidential-vm-required";
    let takes_tdx = mode == "tdx-required" || mode == "confidential-vm-required";
    if !takes_sev_snp && sev_snp_options.values().any(Option::is_some) {
        return Err(GenesisError::validation(format!(
            "tee.mode {mode} forbids SEV-SNP identity values"
        )));
    }

    let mut tdx_options: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for (flag, key) in TDX_OPTION_FLAGS {
        let val = tee.get(key).and_then(|v| match v {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        });
        tdx_options.insert(flag, val);
    }
    if !takes_tdx && tdx_options.values().any(Option::is_some) {
        return Err(GenesisError::validation(format!(
            "tee.mode {mode} forbids TDX identity values"
        )));
    }

    let mut args = vec![
        "tee".to_owned(),
        "genesis".to_owned(),
        "--input".to_owned(),
        seeded_genesis.to_string_lossy().into_owned(),
        "--output".to_owned(),
        output.to_string_lossy().into_owned(),
        "--mode".to_owned(),
        mode.to_owned(),
    ];

    if mode == "dcap-required" {
        let missing: Vec<&str> = dcap_options
            .iter()
            .filter_map(|(&flag, val)| val.is_none().then_some(flag))
            .collect();
        if !missing.is_empty() {
            return Err(GenesisError::validation(format!(
                "tee.mode dcap-required needs exact release values: {}",
                missing.join(", ")
            )));
        }
        for (flag, val) in dcap_options {
            if let Some(value) = val {
                args.push(flag.to_owned());
                args.push(value);
            }
        }
    } else if dcap_options.values().any(Option::is_some) {
        return Err(GenesisError::validation(format!(
            "tee.mode {mode} forbids DCAP measurement values"
        )));
    }
    if takes_sev_snp {
        let missing: Vec<&str> = sev_snp_options
            .iter()
            .filter_map(|(&flag, val)| val.is_none().then_some(flag))
            .collect();
        if !missing.is_empty() {
            return Err(GenesisError::validation(format!(
                "tee.mode {mode} needs the SEV-SNP launch identity: {}",
                missing.join(", ")
            )));
        }
        for (flag, val) in sev_snp_options {
            if let Some(value) = val {
                args.push(flag.to_owned());
                args.push(value);
            }
        }
    }

    if takes_tdx {
        let missing: Vec<&str> = tdx_options
            .iter()
            .filter_map(|(&flag, val)| val.is_none().then_some(flag))
            .collect();
        if !missing.is_empty() {
            return Err(GenesisError::validation(format!(
                "tee.mode {mode} needs the trust domain identity: {}",
                missing.join(", ")
            )));
        }
        for (flag, val) in tdx_options {
            if let Some(value) = val {
                args.push(flag.to_owned());
                args.push(value);
            }
        }
    }

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
