//! SSH orchestration wrapper for `outbe-up vm host-nat`.

use std::{collections::BTreeMap, path::Path};

use eyre::{ensure, Result};
use outbe_operator_node_role::progress::Progress;
use outbe_operator_vm::nat::{
    run_apply_on_host, run_remove_on_host, validate_iface_name, ApplyOutcome, HostNatPaths,
    HostRunner, HostTarget, StageReporter,
};

use crate::network::{
    build::DeploymentKind,
    config::Ssh,
    remote::{NetworkArtifacts, SshOptions},
};

/// Options for `outbe-up vm host-nat`.
pub struct HostNatOptions<'a> {
    pub artifacts: &'a Path,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub uplink: Option<&'a str>,
    pub remove: bool,
    pub dry_run: bool,
    pub force: bool,
}

struct SshHostRunner<'a> {
    ssh: &'a Ssh,
    ssh_options: &'a SshOptions,
}

impl HostRunner for SshHostRunner<'_> {
    fn run_raw(&mut self, host: &str, command: &str) -> Result<String> {
        self.ssh_options.capture_ssh_raw(self.ssh, host, command)
    }
}

struct ProgressStage<'a>(&'a mut Progress);

impl StageReporter for ProgressStage<'_> {
    fn stage(&mut self, message: &str) {
        self.0.stage(message);
    }
}

/// Configure or remove host-side NAT masquerade on every distinct VMM host in the network.
pub fn run(options: HostNatOptions<'_>) -> Result<()> {
    if let Some(uplink) = options.uplink {
        validate_iface_name(uplink, "uplink")?;
    }

    let mut progress = Progress::new();
    progress.stage("loading VM network artifacts");
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure!(
        build.manifest.deployment == DeploymentKind::Vm,
        "network {} is not a VM deployment",
        build.config.name
    );
    let ssh_options = SshOptions::new(options.ssh_key, options.ssh_config)?;
    let targets = group_host_targets(&build)?;
    let paths = HostNatPaths::default();

    if options.remove {
        for (ssh, target) in &targets {
            let mut runner = SshHostRunner {
                ssh,
                ssh_options: &ssh_options,
            };
            let mut reporter = ProgressStage(&mut progress);
            run_remove_on_host(target, &mut runner, &paths, options.dry_run, &mut reporter)?;
        }
        if options.dry_run {
            progress.done(&format!(
                "dry-run complete: no changes made on {} host(s)",
                targets.len()
            ));
        } else {
            progress.done(&format!(
                "removed managed host NAT artifacts on {} host(s)",
                targets.len()
            ));
        }
        return Ok(());
    }

    let mut applied_count = 0usize;
    let mut noop_count = 0usize;
    for (ssh, target) in &targets {
        let mut runner = SshHostRunner {
            ssh,
            ssh_options: &ssh_options,
        };
        let mut reporter = ProgressStage(&mut progress);
        match run_apply_on_host(
            target,
            &mut runner,
            options.uplink,
            &paths,
            options.dry_run,
            options.force,
            options.artifacts,
            &mut reporter,
        )? {
            ApplyOutcome::DryRun => {}
            ApplyOutcome::NoOpExistingEgress => noop_count += 1,
            ApplyOutcome::Applied => applied_count += 1,
        }
    }

    if options.dry_run {
        progress.done(&format!(
            "dry-run complete: no changes made on {} host(s)",
            targets.len()
        ));
    } else if applied_count == 0 && noop_count > 0 {
        progress.done(&format!(
            "no changes needed on {noop_count} host(s) (outbound egress already provided)"
        ));
    } else {
        progress.done(&format!(
            "host NAT complete: {applied_count} applied, {noop_count} already provided"
        ));
    }
    Ok(())
}

fn group_host_targets(build: &NetworkArtifacts) -> Result<Vec<(Ssh, HostTarget)>> {
    let mut grouped: BTreeMap<(String, u16, String), (Ssh, HostTarget)> = BTreeMap::new();
    for founder in &build.config.founders {
        let resolved = build.config.resolve_founder_vm(founder)?;
        validate_iface_name(&resolved.bridge, "bridge")?;
        let key = (
            resolved.vmm_host.host.clone(),
            resolved.vmm_host.port,
            resolved.vmm_host.user.clone(),
        );
        match grouped.get_mut(&key) {
            Some((_, existing)) => {
                ensure!(
                    existing.bridge == resolved.bridge,
                    "VMM host {} has founders on multiple bridges ({} and {}); \
                     `outbe-up vm host-nat` manages one bridge per host",
                    resolved.vmm_host.host,
                    existing.bridge,
                    resolved.bridge
                );
                if !existing.founder_ips.contains(&resolved.bridge_ip) {
                    existing.founder_ips.push(resolved.bridge_ip);
                }
            }
            None => {
                let host = resolved.vmm_host.host.clone();
                grouped.insert(
                    key,
                    (
                        resolved.vmm_host,
                        HostTarget {
                            host,
                            bridge: resolved.bridge,
                            founder_ips: vec![resolved.bridge_ip],
                        },
                    ),
                );
            }
        }
    }
    Ok(grouped.into_values().collect())
}
