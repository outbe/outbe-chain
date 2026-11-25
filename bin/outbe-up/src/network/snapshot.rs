use std::{
    path::{Component, Path},
    time::Duration,
};

use eyre::{ensure, Context, Result};
use outbe_genesis_cli::launch_bundle::quote;

use crate::support::progress::Progress;

use super::{
    lifecycle::{as_root, guard, simplex_quorum, wait_healthy, HealthWait},
    remote::{NetworkArtifacts, SshOptions},
    status::{healthy, query_all},
};

const TARGET: &str = "outbe-validator.target";
const SNAPSHOT_ROOT: &str = "/opt/outbe-chain/snapshots";

pub struct RemoteSnapshotOptions<'a> {
    pub artifacts: &'a Path,
    pub founder: &'a str,
    pub archive: &'a str,
    pub ssh_key: Option<&'a Path>,
    pub timeout: Duration,
    pub skip_dkg_guard: bool,
    pub skip_quorum_guard: bool,
}

pub async fn create(options: RemoteSnapshotOptions<'_>, download: Option<&Path>) -> Result<()> {
    validate_remote_archive(options.archive)?;
    let mut progress = Progress::new();
    progress.stage("checking snapshot maintenance safety");
    let (build, ssh, founder, required_peers) = preflight(&options).await?;
    let validator = format!("/opt/outbe-chain/validator-{}", founder.index);
    let parent = Path::new(options.archive)
        .parent()
        .expect("validated archive has a parent")
        .to_string_lossy();
    let prepare = format!(
        "set -eu; install -d -m 0750 {parent}; test ! -e {archive}; \
required=$(du -sk {data} {consensus} 2>/dev/null | awk '{{sum += $1}} END {{print sum + int(sum / 10)}}'); \
available=$(df -Pk {parent} | awk 'NR==2 {{print $4}}'); \
test -n \"$required\" -a -n \"$available\" -a \"$available\" -ge \"$required\"",
        parent = quote(&parent),
        archive = quote(options.archive),
        data = quote(&format!("{validator}/data")),
        consensus = quote(&format!("{validator}/consensus")),
    );
    ssh.run_quiet(&founder, &as_root(&prepare))
        .wrap_err("snapshot disk preflight failed")?;

    progress.stage(&format!("creating snapshot on {}", founder.name));
    let command = snapshot_command(
        SnapshotAction::Create {
            creator: &founder.validator_address,
            source: &build.manifest.software.commit,
        },
        options.archive,
        founder.index,
    );
    let operation = ssh.run_quiet(&founder, &transient_command(&command));
    let recovery = wait_healthy(
        HealthWait {
            build: &build,
            ssh: &ssh,
            founder: &founder.name,
            required_peers,
            timeout: options.timeout,
        },
        &mut progress,
    )
    .await;
    operation.wrap_err("remote snapshot creation failed")?;
    recovery.wrap_err("snapshot was created, but founder failed to recover")?;
    if let Some(destination) = download {
        progress.stage("downloading snapshot");
        ensure!(
            !destination.exists(),
            "download destination already exists: {}",
            destination.display()
        );
        ssh.copy_root_file_from(&founder, options.archive, destination)?;
    }
    progress.done(&format!(
        "snapshot created on {} at {}",
        founder.name, options.archive
    ));
    Ok(())
}

pub async fn validate(
    options: RemoteSnapshotOptions<'_>,
    expected_signer: Option<&str>,
) -> Result<()> {
    validate_remote_archive(options.archive)?;
    let mut progress = Progress::new();
    progress.stage("checking snapshot validation safety");
    let (build, ssh, founder, required_peers) = preflight(&options).await?;
    ssh.run_quiet(
        &founder,
        &as_root(&format!("test -f {}", quote(options.archive))),
    )?;
    progress.stage(&format!("validating snapshot on {}", founder.name));
    let command = snapshot_command(
        SnapshotAction::Validate { expected_signer },
        options.archive,
        founder.index,
    );
    let operation = ssh.run_quiet(&founder, &transient_command(&command));
    let recovery = wait_healthy(
        HealthWait {
            build: &build,
            ssh: &ssh,
            founder: &founder.name,
            required_peers,
            timeout: options.timeout,
        },
        &mut progress,
    )
    .await;
    operation.wrap_err("remote snapshot validation failed")?;
    recovery.wrap_err("snapshot is valid, but founder failed to recover")?;
    progress.done(&format!("snapshot is valid: {}", options.archive));
    Ok(())
}

async fn preflight(
    options: &RemoteSnapshotOptions<'_>,
) -> Result<(
    NetworkArtifacts,
    SshOptions,
    super::build::FounderArtifact,
    usize,
)> {
    let build = NetworkArtifacts::load(options.artifacts)?;
    let ssh = SshOptions::new(options.ssh_key, &build.manifest.genesis.final_sha256)?;
    let count = build.manifest.founders.len();
    if !options.skip_quorum_guard {
        ensure!(
            count.saturating_sub(1) >= simplex_quorum(count),
            "snapshot interruption would leave too few founders for quorum; pass --skip-quorum-guard only for an intentional outage"
        );
    }
    let required_peers = count.saturating_sub(1);
    let statuses = query_all(&build, &ssh).await?;
    ensure!(
        statuses
            .iter()
            .all(|status| healthy(status, build.manifest.chain_id, required_peers)),
        "all founders must be healthy before a snapshot operation"
    );
    let status = statuses
        .iter()
        .find(|status| status.name == options.founder)
        .ok_or_else(|| eyre::eyre!("unknown founder: {}", options.founder))?;
    guard(status, options.skip_dkg_guard)?;
    let founder = build
        .manifest
        .founders
        .iter()
        .find(|founder| founder.name == options.founder)
        .cloned()
        .expect("status founder exists in manifest");
    Ok((build, ssh, founder, required_peers))
}

fn transient_command(operation: &str) -> String {
    as_root(&format!(
        "systemctl stop {TARGET} || exit $?; ! systemctl is-active --quiet {TARGET} || exit 1; \
rc=0; {operation} || rc=$?; systemctl start {TARGET} || exit $?; exit $rc"
    ))
}

enum SnapshotAction<'a> {
    Create { creator: &'a str, source: &'a str },
    Validate { expected_signer: Option<&'a str> },
}

fn snapshot_command(action: SnapshotAction<'_>, archive: &str, index: usize) -> String {
    let validator = format!("/opt/outbe-chain/validator-{index}");
    let keys = format!("/opt/outbe-chain/keys/validator-{index}");
    let mut args = vec!["/opt/outbe-chain/outbe-chain".to_owned(), "snapshot".into()];
    match action {
        SnapshotAction::Create { creator, source } => {
            args.extend([
                "create".into(),
                "--output".into(),
                archive.into(),
                "--signing-key".into(),
                format!("{keys}/evm-key.hex"),
            ]);
            args.extend(["--creator".into(), creator.into()]);
            args.extend(["--source".into(), source.into()]);
        }
        SnapshotAction::Validate { expected_signer } => {
            args.extend(["validate".into(), "--archive".into(), archive.into()]);
            if let Some(signer) = expected_signer {
                args.extend(["--expected-signer".into(), signer.into()]);
            }
        }
    }
    args.push("--".into());
    args.extend([
        "--config".into(),
        format!("{validator}/reth.toml"),
        "--chain".into(),
        "/opt/outbe-chain/genesis.json".into(),
        "--datadir".into(),
        format!("{validator}/data"),
        "--consensus.storage-dir".into(),
        format!("{validator}/consensus"),
        "--consensus.keys-dir".into(),
        keys.clone(),
        "--p2p-secret-key".into(),
        format!("{keys}/reth-p2p-secret.hex"),
        "--consensus.signing-key".into(),
        format!("{keys}/signing-key.hex"),
        "--validator.evm-key".into(),
        format!("{keys}/evm-key.hex"),
    ]);
    args.into_iter()
        .map(|argument| quote(&argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_remote_archive(value: &str) -> Result<()> {
    let path = Path::new(value);
    ensure!(path.is_absolute(), "remote snapshot path must be absolute");
    ensure!(
        path.starts_with(SNAPSHOT_ROOT),
        "remote snapshot must be below {SNAPSHOT_ROOT}"
    );
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_))),
        "remote snapshot path contains unsafe components"
    );
    ensure!(
        value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_')
        }),
        "remote snapshot path contains unsupported characters"
    );
    ensure!(
        path.file_name().is_some(),
        "remote snapshot path has no file name"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_snapshots_stay_in_the_managed_directory() {
        assert!(validate_remote_archive("/opt/outbe-chain/snapshots/node.tar.zst").is_ok());
        assert!(validate_remote_archive("/tmp/node.tar.zst").is_err());
        assert!(validate_remote_archive("/opt/outbe-chain/snapshots/../keys/x").is_err());
    }

    #[test]
    fn founder_snapshot_uses_the_packaged_key_directory() {
        let command = snapshot_command(
            SnapshotAction::Create {
                creator: "0x01",
                source: "commit",
            },
            "/opt/outbe-chain/snapshots/node.tar.zst",
            2,
        );
        assert!(command.contains("/opt/outbe-chain/keys/validator-2/evm-key.hex"));
        assert!(command.contains("/opt/outbe-chain/keys/validator-2/signing-key.hex"));
        assert!(!command.contains("/opt/outbe-chain/validator-2/evm-key.hex"));
    }
}
