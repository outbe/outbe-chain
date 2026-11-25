use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

use eyre::{ensure, Context, Result};
use outbe_software_bundle::sha256_file;
use serde::Serialize;

use crate::support::process::{self, ROOT};

use super::{
    assets, journal::Journal, rpc::RpcClient, status::sidecar_status, validator_address,
    VALIDATOR_TARGET,
};

const SUPPORT_UNITS: &[&str] = &[
    "outbe-validator.target",
    "outbe-validator.service",
    "outbe-radicle.service",
    "outbe-feeder.service",
    "outbe-hyperlane-validator.service",
];
const SUPPORT_CONFIG_FILES: &[&str] = &["genesis.json", "reth.toml"];

#[derive(Debug, Serialize)]
struct DoctorReport {
    target_active: bool,
    feeder_active: bool,
    feeder_healthy: Option<bool>,
    hyperlane_active: bool,
    hyperlane_healthy: Option<bool>,
    services_enabled: bool,
    systemd_units_current: bool,
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
    next_dkg_freeze_height: Option<u64>,
    maintenance_safe: Option<bool>,
    validator_status: Option<u8>,
    software_bundle_sha256: Option<String>,
    software_commit: Option<String>,
    issues: Vec<String>,
}

struct HostDiagnostics {
    target_active: bool,
    feeder_active: bool,
    hyperlane_active: bool,
    services_enabled: bool,
    systemd_units_current: bool,
    disk_available_kib: Option<u64>,
    time_synchronized: Option<bool>,
    outbe_chain_version: Option<String>,
    genesis_sha256: Option<String>,
}

struct RpcDiagnostics {
    local_chain_id: Option<u64>,
    local_finalized: Option<u64>,
    remote_finalized: Option<u64>,
    execution_peers: Option<u64>,
    consensus_mode: Option<String>,
    consensus_peers: Option<u64>,
    resident_tee_ready: Option<bool>,
    next_dkg_freeze_height: Option<u64>,
    maintenance_safe: Option<bool>,
    validator_status: Option<u8>,
    feeder_healthy: Option<bool>,
    hyperlane_healthy: Option<bool>,
}

pub async fn run(support_bundle: Option<&Path>) -> Result<()> {
    process::require_root()?;
    let journal = Journal::load()?;
    let mut issues = Vec::new();
    let host = collect_host_diagnostics(&mut issues);
    let rpc = collect_rpc_diagnostics(&journal, host.target_active, &mut issues).await?;
    let report = DoctorReport {
        target_active: host.target_active,
        feeder_active: host.feeder_active,
        feeder_healthy: rpc.feeder_healthy,
        hyperlane_active: host.hyperlane_active,
        hyperlane_healthy: rpc.hyperlane_healthy,
        services_enabled: host.services_enabled,
        systemd_units_current: host.systemd_units_current,
        disk_available_kib: host.disk_available_kib,
        time_synchronized: host.time_synchronized,
        outbe_chain_version: host.outbe_chain_version,
        genesis_sha256: host.genesis_sha256,
        expected_chain_id: journal.config.network.chain_id,
        local_chain_id: rpc.local_chain_id,
        local_finalized: rpc.local_finalized,
        remote_finalized: rpc.remote_finalized,
        execution_peers: rpc.execution_peers,
        consensus_mode: rpc.consensus_mode,
        consensus_peers: rpc.consensus_peers,
        resident_tee_ready: rpc.resident_tee_ready,
        next_dkg_freeze_height: rpc.next_dkg_freeze_height,
        maintenance_safe: rpc.maintenance_safe,
        validator_status: rpc.validator_status,
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

fn collect_host_diagnostics(issues: &mut Vec<String>) -> HostDiagnostics {
    let target_active = process::systemctl_is_active(VALIDATOR_TARGET);
    if !target_active {
        issues.push(format!("{VALIDATOR_TARGET} is not active"));
    }
    let feeder_active = process::systemctl_is_active("outbe-feeder.service");
    let hyperlane_active = process::systemctl_is_active("outbe-hyperlane-validator.service");
    if target_active && !feeder_active {
        issues.push("outbe-feeder.service is not active".into());
    }
    if target_active && !hyperlane_active {
        issues.push("outbe-hyperlane-validator.service is not active".into());
    }
    let systemd_units_current = systemd_units_current();
    if !systemd_units_current {
        issues.push("installed systemd units differ from the managed copies".into());
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

    HostDiagnostics {
        target_active,
        feeder_active,
        hyperlane_active,
        services_enabled: process::services_enabled(),
        systemd_units_current,
        disk_available_kib,
        time_synchronized,
        outbe_chain_version,
        genesis_sha256,
    }
}

async fn collect_rpc_diagnostics(
    journal: &Journal,
    target_active: bool,
    issues: &mut Vec<String>,
) -> Result<RpcDiagnostics> {
    let local = RpcClient::new(journal.config.network.local_rpc_url())?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    let address = validator_address(&Path::new(ROOT).join("keys/evm-key.hex"))?;
    let (local_chain_id, local_block, remote_block, peers, consensus, validator, feeder, hyperlane) = tokio::join!(
        local.chain_id(),
        local.finalized_block(),
        remote.finalized_block(),
        local.peer_count(),
        local.consensus_status(),
        remote.validator(address),
        sidecar_status("http://127.0.0.1:9002/status"),
        sidecar_status("http://127.0.0.1:9003/status"),
    );
    let local_chain_id = result_value(local_chain_id, "local chain ID", issues);
    if local_chain_id.is_some_and(|value| value != journal.config.network.chain_id) {
        issues.push("local chain ID does not match operator configuration".into());
    }
    let local_block = result_value(local_block, "local finalized block", issues);
    let remote_block = result_value(remote_block, "remote finalized block", issues);
    if let (Some(local), Some(remote)) = (&local_block, &remote_block) {
        if remote.number.saturating_sub(local.number) > 16 {
            issues.push(format!(
                "validator is {} finalized blocks behind",
                remote.number.saturating_sub(local.number)
            ));
        }
    }
    let peers = result_value(peers, "execution peer count", issues);
    if target_active && peers == Some(0) {
        issues.push("validator has no execution peers".into());
    }
    let consensus = result_value(consensus, "consensus status", issues);
    if target_active
        && consensus
            .as_ref()
            .is_some_and(|status| !status.resident_ready())
    {
        issues.push("resident TEE is not ready".into());
    }
    let validator = result_value(validator, "on-chain validator record", issues);
    let feeder_healthy = sidecar_health(
        feeder,
        "feeder status",
        target_active,
        "feeder is not healthy",
        issues,
    );
    let hyperlane_healthy = sidecar_health(
        hyperlane,
        "Hyperlane status",
        target_active,
        "Hyperlane validator is not healthy",
        issues,
    );

    Ok(RpcDiagnostics {
        local_chain_id,
        local_finalized: local_block.as_ref().map(|block| block.number),
        remote_finalized: remote_block.as_ref().map(|block| block.number),
        execution_peers: peers,
        consensus_mode: consensus.as_ref().map(|status| status.mode.clone()),
        consensus_peers: consensus.as_ref().map(|status| status.connected_peers),
        resident_tee_ready: consensus.as_ref().map(|status| status.resident_ready()),
        next_dkg_freeze_height: consensus
            .as_ref()
            .and_then(|status| status.next_dkg_freeze_height),
        maintenance_safe: consensus
            .as_ref()
            .and_then(|status| status.maintenance_safe),
        validator_status: validator.as_ref().map(|record| record.status),
        feeder_healthy,
        hyperlane_healthy,
    })
}

fn sidecar_health(
    result: Result<serde_json::Value>,
    label: &str,
    target_active: bool,
    unhealthy_issue: &str,
    issues: &mut Vec<String>,
) -> Option<bool> {
    let status = result_value(result, label, issues);
    let healthy = status
        .as_ref()
        .and_then(|status| status.get("healthy"))
        .and_then(serde_json::Value::as_bool);
    if target_active && status.is_some() && healthy != Some(true) {
        issues.push(unhealthy_issue.into());
    }
    healthy
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
        if value.trim() == "yes" {
            return Some(true);
        }
    }
    process::capture("chronyc", &["tracking"])
        .ok()
        .map(|value| value.contains("Leap status") && value.contains("Normal"))
}

fn systemd_units_current() -> bool {
    assets::systemd_units()
        .is_ok_and(|expected| units_match_expected(Path::new("/etc/systemd/system"), &expected))
}

fn units_match_expected(installed_dir: &Path, expected: &[(String, String)]) -> bool {
    expected.iter().all(|(name, content)| {
        fs::read(installed_dir.join(name)).is_ok_and(|installed| installed == content.as_bytes())
    })
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
    for &name in SUPPORT_CONFIG_FILES {
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

    #[test]
    fn support_bundle_excludes_configs_that_may_contain_url_credentials() {
        for name in [
            "validator.env",
            "feeder-public.toml",
            "hyperlane-public.toml",
        ] {
            assert!(!SUPPORT_CONFIG_FILES.contains(&name));
        }
    }

    #[test]
    fn unit_drift_is_compared_with_canonical_content() {
        let installed = tempfile::tempdir().unwrap();
        let expected = vec![("outbe-validator.service".to_owned(), "canonical".to_owned())];
        fs::write(
            installed.path().join("outbe-validator.service"),
            "canonical",
        )
        .unwrap();
        assert!(units_match_expected(installed.path(), &expected));
        fs::write(installed.path().join("outbe-validator.service"), "stale").unwrap();
        assert!(!units_match_expected(installed.path(), &expected));
    }
}
