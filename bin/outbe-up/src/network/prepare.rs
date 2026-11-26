use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use eyre::{ensure, Context, Result};
use outbe_genesis_cli::launch_bundle::quote;
use outbe_software_bundle::verify_and_extract;
use tokio::task::JoinSet;
use walkdir::WalkDir;

use outbe_operator_node_role::progress::Progress;

use super::{
    build::DeploymentKind,
    remote::{NetworkArtifacts, SshOptions},
};

const INSTALLER: &str = r#"#!/bin/sh
set -eu

index="${1:?validator index is required}"
archive_sha="${2:?archive digest is required}"
genesis_sha="${3:?genesis digest is required}"
archive="validator-${index}.tgz"
base=/opt/outbe-chain

as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  else
    sudo -n -- "$@"
  fi
}

[ "$(sha256sum "$archive" | cut -d ' ' -f 1)" = "$archive_sha" ] || {
  echo "founder archive checksum mismatch" >&2
  exit 1
}
if systemctl is-active --quiet outbe-validator.target; then
  echo "outbe-validator.target is active; stop the existing network before prepare" >&2
  exit 1
fi
if as_root test -f "$base/genesis.json"; then
  current="$(as_root sha256sum "$base/genesis.json" | cut -d ' ' -f 1)"
  [ "$current" = "$genesis_sha" ] || {
    echo "a different genesis is already installed at $base" >&2
    exit 1
  }
fi

as_root install -d -m 0755 "$base"
for binary in bin/*; do
  [ -f "$binary" ] || continue
  as_root install -m 0755 "$binary" "$base/$(basename "$binary")"
done
as_root tar xzf "$archive" --no-same-owner -C "$base"
as_root "$base/validator-${index}/preflight.sh"
as_root "$base/validator-${index}/install-systemd.sh" --prepare-only
"#;

pub async fn run(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    _ssh_config: Option<&Path>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("verifying network artifacts and software bundle");
    let build = NetworkArtifacts::load(artifacts)?;
    ensure!(
        build.manifest.deployment == DeploymentKind::Host,
        "network prepare only supports bare-host deployments; use outbe-up vm disk / vm create for VM deployments"
    );
    let ssh = SshOptions::new(ssh_key, &build.manifest.genesis.final_sha256)?;
    let temporary = tempfile::tempdir()?;
    let software = temporary.path().join("software");
    fs::create_dir(&software)?;
    verify_and_extract(&build.root.join("software-bundle.tar.zst"), &software)?;
    ensure!(
        software.join("bin/outbe-up").is_file(),
        "software bundle has no outbe-up binary"
    );
    let installer = temporary.path().join("install-founder.sh");
    fs::write(&installer, INSTALLER)?;
    fs::set_permissions(&installer, fs::Permissions::from_mode(0o700))?;
    let genesis_sha = build.manifest.genesis.final_sha256.clone();

    progress.stage(&format!(
        "preparing {} founding validators",
        build.manifest.founders.len()
    ));
    let mut tasks = JoinSet::new();
    for founder in build.manifest.founders.clone() {
        let ssh = ssh.clone();
        let index = founder.index;
        let archive_rel = founder
            .archive
            .as_deref()
            .ok_or_else(|| eyre::eyre!("founder {index} manifest has no archive"))?;
        let archive = build.root.join(archive_rel);
        let archive_sha = founder
            .archive_sha256
            .clone()
            .ok_or_else(|| eyre::eyre!("founder {index} manifest has no archive_sha256"))?;
        let binaries = software.join("bin");
        let installer = installer.clone();
        let genesis_sha = genesis_sha.clone();
        let required_kib = required_disk_kib(&binaries, &archive)?;
        tasks.spawn_blocking(move || {
            prepare_founder(PrepareFounder {
                ssh: &ssh,
                founder: &founder,
                index,
                binaries: &binaries,
                archive: &archive,
                installer: &installer,
                archive_sha: &archive_sha,
                genesis_sha: &genesis_sha,
                required_kib,
            })
            .wrap_err_with(|| format!("prepare {}", founder.name))
        });
    }

    let mut completed = 0;
    while let Some(result) = tasks.join_next().await {
        result.wrap_err("founder prepare task panicked")??;
        completed += 1;
        progress.update(&format!(
            "preparing founding validators ({completed}/{} complete)",
            build.manifest.founders.len()
        ));
    }
    progress.done(&format!(
        "prepared {completed} founding validators; services remain disabled and stopped"
    ));
    Ok(())
}

struct PrepareFounder<'a> {
    ssh: &'a SshOptions,
    founder: &'a super::build::FounderArtifact,
    index: usize,
    binaries: &'a Path,
    archive: &'a Path,
    installer: &'a Path,
    archive_sha: &'a str,
    genesis_sha: &'a str,
    required_kib: u64,
}

fn prepare_founder(input: PrepareFounder<'_>) -> Result<()> {
    let private_ip = input.founder.private_ip;
    let ports = &input.founder.ports;
    let required_ports = [
        8_545,
        ports.reth_p2p,
        ports.reth_discovery,
        ports.consensus,
        ports.radicle,
        8_551,
        8_876,
        9_002,
        9_101,
        9_000,
        9_003,
    ]
    .map(|port| port.to_string())
    .join(" ");
    let remote = input.ssh.capture(
        input.founder,
        &format!(
            "set -eu; test \"$(uname -s)\" = Linux; test \"$(uname -m)\" = x86_64; \
command -v systemctl >/dev/null; command -v tar >/dev/null; command -v sha256sum >/dev/null; \
command -v curl >/dev/null; command -v ip >/dev/null; command -v ss >/dev/null; \
if [ \"$(id -u)\" -ne 0 ]; then command -v sudo >/dev/null; sudo -n true; fi; \
ip -4 -o addr show | grep -Fq ' {private_ip}/' \
  || {{ echo 'private_ip {private_ip} is not configured on this VM' >&2; exit 1; }}; \
for port in {required_ports}; do \
  ! ss -H -lntu | grep -Eq \"[:.]$port[[:space:]]\" \
    || {{ echo \"required port $port is already in use\" >&2; exit 1; }}; \
done; \
available=\"$(df -Pk /opt 2>/dev/null | awk 'NR==2 {{print $4}}')\"; \
[ -n \"$available\" ] || available=\"$(df -Pk / | awk 'NR==2 {{print $4}}')\"; \
[ \"$available\" -ge {} ] \
  || {{ echo 'not enough free disk space for the founder bundle' >&2; exit 1; }}; \
mktemp -d /tmp/outbe-network.XXXXXX",
            input.required_kib
        ),
    )?;
    ensure!(
        remote.starts_with("/tmp/outbe-network.")
            && remote
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'/' | b'.' | b'-' | b'_')),
        "remote host returned an unsafe staging path"
    );
    let remote_quoted = quote(&remote);
    let result = (|| {
        input
            .ssh
            .copy_to(input.founder, &[input.binaries], &remote, true)?;
        input.ssh.copy_to(
            input.founder,
            &[input.archive, input.installer],
            &remote,
            false,
        )?;
        input.ssh.run_quiet(
            input.founder,
            &format!(
                "cd {remote_quoted} && sh install-founder.sh {} {} {}",
                input.index, input.archive_sha, input.genesis_sha
            ),
        )
    })();
    let cleanup = input
        .ssh
        .run(input.founder, &format!("rm -rf -- {remote_quoted}"));
    result?;
    cleanup?;
    Ok(())
}

fn required_disk_kib(binaries: &Path, archive: &Path) -> Result<u64> {
    let mut bytes = std::fs::metadata(archive)?.len();
    for entry in WalkDir::new(binaries).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            bytes = bytes.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(bytes.saturating_mul(2).div_ceil(1024) + 1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_never_starts_the_network() {
        assert!(INSTALLER.contains("--prepare-only"));
        assert!(INSTALLER.contains("outbe-validator.target is active"));
        assert!(!INSTALLER.contains("enable --now"));
    }
}
