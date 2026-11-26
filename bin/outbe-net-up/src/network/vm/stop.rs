//! Multi-host VM network stop orchestration (`outbe-net-up network stop`).

use std::{collections::BTreeMap, path::Path, sync::Arc};

use eyre::{Context, Result};
use outbe_operator_node_role::progress::Progress;
use outbe_operator_remote::{run_host_json, shell_quote, HostExec, Ssh};
use tokio::task::JoinSet;

use super::{
    ensure_vm_deployment,
    remote_bin::{
        ensure_pinned_outbe_up_on_host, fetch_host_vms, resolve_state_or_bundled_outbe_up_sha256,
        RemoteOutbeUp,
    },
    shell_quote_tilde_path,
    types::{
        build_all_desired_members, collect_unique_hosts, network_owner_tag_prefix,
        RemoteVmStatusOutput, RemoteVmStopOutput, UniqueHost,
    },
    StopOptions,
};
use crate::network::{remote::NetworkArtifacts, vm_state::NetworkVmState};

type StopTarget = (Ssh, String, RemoteOutbeUp, RemoteVmStatusOutput);

pub(crate) async fn stop_with_exec(
    options: &StopOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("verifying pinned outbe-up and discovering network VMs on hosts");
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;

    let state = {
        let lock = NetworkVmState::lock(options.artifacts)?;
        NetworkVmState::load_optional_locked(&lock, options.artifacts)?
    };
    if let Some(st) = &state {
        st.ensure_matches_network(&build)?;
    }
    let outbe_up_sha256 = resolve_state_or_bundled_outbe_up_sha256(&build.root, state.as_ref())?;
    let desired_members = build_all_desired_members(&build, state.as_ref(), None, None, None)?;
    let unique_hosts = collect_unique_hosts(&desired_members);
    let tag_prefix =
        network_owner_tag_prefix(&build.config.name, &build.manifest.genesis.final_sha256);

    let stoppable = discover_stoppable_vms(
        exec.as_ref(),
        &build.root,
        &outbe_up_sha256,
        &unique_hosts,
        &tag_prefix,
    )?;

    let mode_label = if options.force {
        "force-stopping"
    } else {
        "gracefully stopping"
    };
    progress.stage(&format!(
        "{mode_label} {} network VM(s) in parallel",
        stoppable.len()
    ));

    let stopped =
        stop_vms_in_parallel(stoppable, options.force, options.timeout.as_secs(), &exec).await?;
    let done_verb = if options.force {
        "force-stopped"
    } else {
        "gracefully stopped"
    };
    progress.done(&format!("{done_verb} {stopped} network VM(s)"));
    Ok(())
}

fn discover_stoppable_vms(
    exec: &dyn HostExec,
    artifacts_root: &Path,
    outbe_up_sha256: &str,
    unique_hosts: &BTreeMap<String, UniqueHost>,
    tag_prefix: &str,
) -> Result<Vec<StopTarget>> {
    let mut stoppable = Vec::new();
    for host in unique_hosts.values() {
        let bin =
            ensure_pinned_outbe_up_on_host(exec, &host.ssh, artifacts_root, outbe_up_sha256, true)?;
        for vm in fetch_host_vms(exec, &host.ssh, &bin, &host.vmm_home)? {
            let is_stoppable = vm
                .owner_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(tag_prefix))
                && !vm.vm_id.is_empty()
                && vm.phase != "creating";
            if is_stoppable {
                stoppable.push((host.ssh.clone(), host.vmm_home.clone(), bin.clone(), vm));
            }
        }
    }
    Ok(stoppable)
}

async fn stop_vms_in_parallel(
    stoppable: Vec<StopTarget>,
    force: bool,
    timeout_secs: u64,
    exec: &Arc<dyn HostExec>,
) -> Result<usize> {
    let mut tasks = JoinSet::new();
    for (ssh, vmm_home, bin, vm) in stoppable {
        let exec = Arc::clone(exec);
        tasks.spawn_blocking(move || {
            let mut inner = format!(
                "vm stop --name {} --expect-vm-id {} --timeout {} --vmm-home {}",
                shell_quote(&vm.name),
                shell_quote(&vm.vm_id),
                timeout_secs,
                shell_quote_tilde_path(&vmm_home),
            );
            if force {
                inner.push_str(" --force");
            }
            inner.push_str(" --json");
            let cmd = bin.cmd(&inner);
            let out: RemoteVmStopOutput = run_host_json(exec.as_ref(), &ssh, &vm.name, &cmd)
                .wrap_err_with(|| {
                    format!("stop VM `{}` ({}) on {}", vm.name, vm.vm_id, ssh.host)
                })?;
            Ok::<Option<String>, eyre::Report>(out.warning)
        });
    }

    let mut stopped = 0usize;
    while let Some(result) = tasks.join_next().await {
        if let Some(warning) = result.wrap_err("VM stop task panicked")?? {
            eprintln!("{warning}");
        }
        stopped += 1;
    }
    Ok(stopped)
}
