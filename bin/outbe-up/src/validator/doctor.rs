use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

use eyre::{ensure, Context, Result};
use outbe_software_bundle::sha256_file;
use serde::Serialize;

use crate::support::process::{self, ROOT};

use super::{journal::Journal, rpc::RpcClient, validator_address, VALIDATOR_TARGET};

const SUPPORT_UNITS: &[&str] = &[
    "outbe-validator.target",
    "outbe-validator.service",
    "outbe-radicle.service",
    "outbe-feeder.service",
    "outbe-hyperlane-validator.service",
];

#[derive(Debug, Serialize)]
struct DoctorReport {
    target_active: bool,
    services_enabled: bool,
    disk_available_kib: Option<u64>,
    time_synchronized: Option<bool>,
    outbe_chain_version: Option<String>,
    genesis_sha256: Option<String>,
    expected_chain_id: u64,
    local_chain_id: Option<u64>,
    local_finalized: Option<u64>,
    remote_finalized: Option<u64>,
    execution_peers: Option<u64>,
    consensus_mode: Option<String>,
    consensus_peers: Option<u64>,
    resident_tee_ready: Option<bool>,
    validator_status: Option<u8>,
    software_bundle_sha256: Option<String>,
    software_commit: Option<String>,
    issues: Vec<String>,
}

pub async fn run(support_bundle: Option<&Path>) -> Result<()> {
    process::require_root()?;
    let journal = Journal::load()?;
    let mut issues = Vec::new();
    let target_active = process::systemctl_is_active(VALIDATOR_TARGET);
    if !target_active {
        issues.push(format!("{VALIDATOR_TARGET} is not active"));
    }

    let disk_available_kib = disk_available_kib(Path::new(ROOT));
    if disk_available_kib.is_none() {
        issues.push("cannot determine available disk space".into());
    }
    let time_synchronized = time_synchronized();
    if time_synchronized != Some(true) {
        issues.push("system clock is not confirmed synchronized".into());
    }
    let outbe_chain_version = process::capture(Path::new(ROOT).join("outbe-chain"), &["--version"])
        .ok()
        .map(|value| value.trim().to_owned());
    if outbe_chain_version.is_none() {
        issues.push("cannot read outbe-chain version".into());
    }
    let genesis_sha256 = sha256_file(&Path::new(ROOT).join("genesis.json")).ok();
    if genesis_sha256.is_none() {
        issues.push("cannot hash genesis.json".into());
    }

    let local = RpcClient::new(journal.config.network.local_rpc_url())?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    let address = validator_address(&Path::new(ROOT).join("keys/evm-key.hex"))?;
    let (local_chain_id, local_block, remote_block, peers, consensus, validator) = tokio::join!(
        local.chain_id(),
        local.finalized_block(),
        remote.finalized_block(),
        local.peer_count(),
        local.consensus_status(),
        remote.validator(address),
    );
    let local_chain_id = result_value(local_chain_id, "local chain ID", &mut issues);
    if local_chain_id.is_some_and(|value| value != journal.config.network.chain_id) {
        issues.push("local chain ID does not match operator configuration".into());
    }
    let local_block = result_value(local_block, "local finalized block", &mut issues);
    let remote_block = result_value(remote_block, "remote finalized block", &mut issues);
    if let (Some(local), Some(remote)) = (&local_block, &remote_block) {
        if remote.number.saturating_sub(local.number) > 16 {
            issues.push(format!(
                "validator is {} finalized blocks behind",
                remote.number.saturating_sub(local.number)
            ));
        }
    }
    let peers = result_value(peers, "execution peer count", &mut issues);
    if target_active && peers == Some(0) {
        issues.push("validator has no execution peers".into());
    }
    let consensus = result_value(consensus, "consensus status", &mut issues);
    if target_active
        && consensus
            .as_ref()
            .is_some_and(|status| !status.resident_ready())
    {
        issues.push("resident TEE is not ready".into());
    }
    let validator = result_value(validator, "on-chain validator record", &mut issues);

    let report = DoctorReport {
        target_active,
        services_enabled: process::services_enabled(),
        disk_available_kib,
        time_synchronized,
        outbe_chain_version,
        genesis_sha256,
        expected_chain_id: journal.config.network.chain_id,
        local_chain_id,
        local_finalized: local_block.as_ref().map(|block| block.number),
        remote_finalized: remote_block.as_ref().map(|block| block.number),
        execution_peers: peers,
        consensus_mode: consensus.as_ref().map(|status| status.mode.clone()),
        consensus_peers: consensus.as_ref().map(|status| status.connected_peers),
        resident_tee_ready: consensus.as_ref().map(|status| status.resident_ready()),
        validator_status: validator.as_ref().map(|record| record.status),
        software_bundle_sha256: journal.software_bundle_sha256.clone(),
        software_commit: journal.software_commit.clone(),
        issues,
    };
    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");
    if let Some(path) = support_bundle {
        create_support_bundle(path, &json)?;
        println!("support bundle: {}", path.display());
    }
    ensure!(
        report.issues.is_empty(),
        "validator doctor found {} issue(s)",
        report.issues.len()
    );
    Ok(())
}

fn result_value<T>(result: Result<T>, label: &str, issues: &mut Vec<String>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            issues.push(format!("{label} unavailable: {error:#}"));
            None
        }
    }
}

fn disk_available_kib(path: &Path) -> Option<u64> {
    let output = process::capture("df", &["-Pk", path.to_str()?]).ok()?;
    output
        .lines()
        .last()?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()
}

fn time_synchronized() -> Option<bool> {
    if let Ok(value) = process::capture(
        "timedatectl",
        &["show", "--property=NTPSynchronized", "--value"],
    ) {
        return match value.trim() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        };
    }
    process::capture("chronyc", &["tracking"])
        .ok()
        .map(|value| value.contains("Leap status") && value.contains("Normal"))
}

fn create_support_bundle(output: &Path, report: &str) -> Result<()> {
    ensure!(
        !output.exists(),
        "support bundle already exists: {}",
        output.display()
    );
    let staging = tempfile::tempdir()?;
    fs::write(staging.path().join("doctor.json"), report)?;
    let config = staging.path().join("config");
    fs::create_dir(&config)?;
    for name in [
        "genesis.json",
        "validator.env",
        "feeder-public.toml",
        "hyperlane-public.toml",
        "reth.toml",
    ] {
        copy_public_file(&Path::new(ROOT).join(name), &config.join(name))?;
    }
    let units = staging.path().join("systemd");
    fs::create_dir(&units)?;
    for unit in SUPPORT_UNITS {
        copy_public_file(
            &Path::new(ROOT).join("systemd").join(unit),
            &units.join(unit),
        )?;
    }
    let logs = staging.path().join("journald.txt");
    let mut args = vec!["--no-pager", "--since", "24 hours ago", "-n", "2000"];
    for unit in SUPPORT_UNITS {
        args.extend(["-u", unit]);
    }
    let journal = process::capture("journalctl", &args)
        .unwrap_or_else(|error| format!("journal unavailable: {error:#}\n"));
    fs::write(logs, journal)?;
    write_archive(staging.path(), output)
}

fn copy_public_file(source: &Path, destination: &Path) -> Result<()> {
    match fs::symlink_metadata(source) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "refusing non-regular support file: {}",
                source.display()
            );
            ensure!(
                metadata.len() <= 16 * 1024 * 1024,
                "support file is unexpectedly large: {}",
                source.display()
            );
            fs::copy(source, destination)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) fn write_archive(source: &Path, output: &Path) -> Result<()> {
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .wrap_err_with(|| format!("create support bundle: {}", output.display()))?;
    let encoder = zstd::Encoder::new(file, 9)?;
    let mut archive = tar::Builder::new(encoder);
    archive.append_dir_all(".", source)?;
    let encoder = archive.into_inner()?;
    encoder.finish()?.sync_all()?;
    fs::set_permissions(output, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_portable_df_output() {
        let line = "/dev/vda 100000 25000 75000 25% /opt";
        assert_eq!(line.split_whitespace().nth(3).unwrap(), "75000");
    }

    #[test]
    fn support_bundle_is_private_and_contains_only_staging_tree() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::write(source.path().join("doctor.json"), "{}").unwrap();
        let output = destination.path().join("support.tar.zst");
        write_archive(source.path(), &output).unwrap();
        assert_eq!(
            fs::metadata(output).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
