use std::{collections::BTreeSet, fs, path::Path};

use eyre::{ensure, Result};

use crate::{support::progress::Progress, validator::doctor::write_archive};

use super::{
    build::FounderArtifact,
    remote::{NetworkArtifacts, SshOptions},
    status::{healthy, query_all, render, FounderStatus},
};

const SUPPORT_UNITS: &[&str] = &[
    "outbe-validator.target",
    "outbe-validator.service",
    "outbe-radicle.service",
    "outbe-feeder.service",
    "outbe-hyperlane-validator.service",
];

struct HostDiagnostic<'a> {
    founder: &'a FounderArtifact,
    output: String,
}

struct NetworkDiagnostics<'a> {
    issues: Vec<String>,
    hosts: Vec<HostDiagnostic<'a>>,
}

pub async fn run(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    support_bundle: Option<&Path>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("running network diagnostics");
    let build = NetworkArtifacts::load(artifacts)?;
    let ssh = SshOptions::new(ssh_key, &build.manifest.genesis.final_sha256)?;
    let statuses = query_all(&build, &ssh).await?;
    let required_peers = build.manifest.founders.len().saturating_sub(1);
    let diagnostics = collect_diagnostics(&build, &ssh, &statuses, required_peers);

    progress.done(&format!("diagnosed {} founding validators", statuses.len()));
    let status_text = render(&build, &statuses);
    println!("\n{status_text}");
    for host in &diagnostics.hosts {
        println!("\n{} host diagnostics:\n{}", host.founder.name, host.output);
    }
    if let Some(output) = support_bundle {
        create_support_bundle(&build, &ssh, &diagnostics, &status_text, output)?;
        println!("support bundle: {}", output.display());
    }
    ensure!(
        diagnostics.issues.is_empty(),
        "network doctor found {} issue(s): {}",
        diagnostics.issues.len(),
        diagnostics.issues.join("; ")
    );
    Ok(())
}

fn collect_diagnostics<'a>(
    build: &'a NetworkArtifacts,
    ssh: &SshOptions,
    statuses: &[FounderStatus],
    required_peers: usize,
) -> NetworkDiagnostics<'a> {
    let mut issues = Vec::new();
    let mut hosts = Vec::new();
    let mut versions = BTreeSet::new();
    for (founder, status) in build.manifest.founders.iter().zip(statuses) {
        if !healthy(status, build.manifest.chain_id, required_peers) {
            issues.push(format!("{} is not healthy", founder.name));
        }
        let output = match ssh.capture(founder, &host_diagnostics_command()) {
            Ok(output) => output,
            Err(error) => {
                issues.push(format!(
                    "{} host diagnostics failed: {error:#}",
                    founder.name
                ));
                hosts.push(HostDiagnostic {
                    founder,
                    output: format!("diagnostics unavailable: {error:#}"),
                });
                continue;
            }
        };
        inspect_host_output(founder, &output, build, &mut issues, &mut versions);
        hosts.push(HostDiagnostic { founder, output });
    }
    if versions.len() > 1 {
        issues.push(format!(
            "founders run different outbe-chain versions: {}",
            versions.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    NetworkDiagnostics { issues, hosts }
}

fn inspect_host_output(
    founder: &FounderArtifact,
    output: &str,
    build: &NetworkArtifacts,
    issues: &mut Vec<String>,
    versions: &mut BTreeSet<String>,
) {
    if !output.contains(&format!(
        "genesis_sha256={}",
        build.manifest.genesis.final_sha256
    )) {
        issues.push(format!(
            "{} has a different or unreadable genesis",
            founder.name
        ));
    }
    if !output.contains("time_synchronized=yes") {
        issues.push(format!(
            "{} clock is not confirmed synchronized",
            founder.name
        ));
    }
    if !output.contains("systemd_units_current=yes") {
        issues.push(format!(
            "{} has stale or missing systemd units",
            founder.name
        ));
    }
    if !output.lines().any(|line| {
        line.strip_prefix("disk_available_kib=")
            .is_some_and(|value| value.parse::<u64>().is_ok())
    }) {
        issues.push(format!("{} disk availability is unreadable", founder.name));
    }
    match diagnostic_value(output, "outbe_chain_version") {
        Some(version) if !version.is_empty() => {
            versions.insert(version.to_owned());
        }
        _ => issues.push(format!("{} version is unreadable", founder.name)),
    }
}

fn create_support_bundle(
    build: &NetworkArtifacts,
    ssh: &SshOptions,
    diagnostics: &NetworkDiagnostics<'_>,
    status_text: &str,
    output: &Path,
) -> Result<()> {
    let staging = tempfile::tempdir()?;
    fs::write(staging.path().join("network-status.txt"), status_text)?;
    fs::write(
        staging.path().join("issues.txt"),
        if diagnostics.issues.is_empty() {
            "none\n".to_owned()
        } else {
            format!("{}\n", diagnostics.issues.join("\n"))
        },
    )?;
    let hosts = staging.path().join("hosts");
    fs::create_dir(&hosts)?;
    for host in &diagnostics.hosts {
        let name = safe_name(&host.founder.name);
        fs::write(hosts.join(format!("{name}-diagnostics.txt")), &host.output)?;
        let logs = ssh
            .capture(host.founder, &journal_command())
            .unwrap_or_else(|error| format!("journal unavailable: {error:#}"));
        fs::write(hosts.join(format!("{name}-journald.txt")), logs)?;
    }
    for name in ["network.yaml", "network-manifest.json", "genesis.json"] {
        fs::copy(build.root.join(name), staging.path().join(name))?;
    }
    write_archive(staging.path(), output)
}

fn host_diagnostics_command() -> String {
    "set -u; \
disk=$(df -Pk /opt/outbe-chain 2>/dev/null | awk 'NR==2 {print $4}'); \
time_sync=$(timedatectl show --property=NTPSynchronized --value 2>/dev/null || true); \
if [ \"$time_sync\" = yes ]; then time_sync=yes; \
elif command -v chronyc >/dev/null 2>&1 && chronyc tracking 2>/dev/null | grep -q 'Leap status.*Normal'; then time_sync=yes; \
else time_sync=no; fi; \
printf 'disk_available_kib=%s\\ntime_synchronized=%s\\n' \"$disk\" \"$time_sync\"; \
units_ok=yes; \
for unit in outbe-validator.target outbe-validator.service outbe-radicle.service outbe-feeder.service outbe-hyperlane-validator.service; do cmp -s \"/opt/outbe-chain/systemd/$unit\" \"/etc/systemd/system/$unit\" || units_ok=no; done; \
printf 'systemd_units_current=%s\\n' \"$units_ok\"; \
printf 'outbe_chain_version='; /opt/outbe-chain/outbe-chain --version 2>/dev/null || true; \
printf 'genesis_sha256='; sha256sum /opt/outbe-chain/genesis.json 2>/dev/null | cut -d ' ' -f 1 || true"
        .to_owned()
}

fn journal_command() -> String {
    let mut command = "journalctl --no-pager --since '24 hours ago' -n 2000".to_owned();
    for unit in SUPPORT_UNITS {
        command.push_str(" -u ");
        command.push_str(unit);
    }
    command
}

fn diagnostic_value<'a>(output: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=");
    output.lines().find_map(|line| line.strip_prefix(&prefix))
}

fn safe_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn support_file_names_cannot_escape() {
        assert_eq!(safe_name("../../validator 0"), "______validator_0");
    }

    #[test]
    fn reads_named_diagnostic_values() {
        let output = "disk_available_kib=10\noutbe_chain_version=outbe-chain 1.2.3\n";
        assert_eq!(
            diagnostic_value(output, "outbe_chain_version"),
            Some("outbe-chain 1.2.3")
        );
    }
}
