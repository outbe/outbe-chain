//! Multi-host VM network removal orchestration (`outbe-net-up network remove`).

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::progress::Progress;
use outbe_operator_remote::{run_host_json, shell_quote, HostExec, Ssh};
use tokio::task::JoinSet;

use super::{
    ensure_vm_deployment, host_key,
    remote_bin::{
        ensure_pinned_outbe_up_on_host, fetch_host_staged_disks, fetch_host_vms,
        resolve_state_or_bundled_outbe_up_sha256, RemoteOutbeUp,
    },
    shell_quote_tilde_path,
    types::{
        build_all_desired_members, collect_unique_hosts, network_owner_tag_prefix, DesiredMember,
        RemoteCleanStagingOutput, RemoteCleanStaleOutput, RemoteVmRemoveOutput,
        RemoteVmStatusOutput, UniqueHost,
    },
    RemoveOptions,
};
use crate::network::{remote::NetworkArtifacts, vm_state::NetworkVmState};

type TaggedVmTarget = (Ssh, String, RemoteOutbeUp, RemoteVmStatusOutput);

pub(crate) async fn remove_with_exec(
    options: &RemoveOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("verifying pinned outbe-up and discovering network VMs on hosts");
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;

    let state_lock = NetworkVmState::lock(options.artifacts)?;
    let state = NetworkVmState::load_optional_locked(&state_lock, options.artifacts)?;
    if let Some(st) = &state {
        st.ensure_matches_network(&build)?;
    }
    let outbe_up_sha256 = resolve_state_or_bundled_outbe_up_sha256(&build.root, state.as_ref())?;

    let desired_members = build_all_desired_members(&build, state.as_ref(), None, None, None)?;
    let unique_hosts = collect_unique_hosts(&desired_members);
    let tag_prefix =
        network_owner_tag_prefix(&build.config.name, &build.manifest.genesis.final_sha256);

    let (host_bins, to_remove) = discover_tagged_vms(
        exec.as_ref(),
        &build.root,
        &outbe_up_sha256,
        &unique_hosts,
        &tag_prefix,
    )?;

    progress.stage(&format!(
        "removing {} tagged network VM(s) (graceful={}, leaving host config and guest images untouched)",
        to_remove.len(),
        !options.force
    ));
    let mut remove_errors =
        remove_vms_in_parallel(&to_remove, options.force, options.timeout.as_secs(), &exec).await?;

    progress.stage("cleaning staged raw disks and stale temporary directories on hosts");
    clean_staging_on_hosts(
        exec.as_ref(),
        &desired_members,
        &unique_hosts,
        &host_bins,
        &mut remove_errors,
    );

    if !remove_errors.is_empty() {
        bail!(
            "partial network removal (keeping {} for retry): {}",
            NetworkVmState::path(options.artifacts).display(),
            remove_errors.join("; ")
        );
    }

    verify_zero_remaining_resources(
        exec.as_ref(),
        &build,
        &desired_members,
        &unique_hosts,
        &host_bins,
    )?;

    let deleted_state = NetworkVmState::delete_locked(&state_lock, options.artifacts)?;
    let state_msg = if deleted_state {
        format!(
            "deleted {}",
            NetworkVmState::path(options.artifacts).display()
        )
    } else {
        "no local state file present".to_owned()
    };
    progress.done(&format!(
        "removed {} network VM(s), cleaned staging, {state_msg} (host config and guest images untouched)",
        to_remove.len(),
    ));
    Ok(())
}

fn discover_tagged_vms(
    exec: &dyn HostExec,
    artifacts_root: &Path,
    outbe_up_sha256: &str,
    unique_hosts: &BTreeMap<String, UniqueHost>,
    tag_prefix: &str,
) -> Result<(BTreeMap<String, RemoteOutbeUp>, Vec<TaggedVmTarget>)> {
    let mut host_bins = BTreeMap::new();
    let mut to_remove = Vec::new();
    for (hkey, host) in unique_hosts {
        let bin =
            ensure_pinned_outbe_up_on_host(exec, &host.ssh, artifacts_root, outbe_up_sha256, true)?;
        let vms = fetch_host_vms(exec, &host.ssh, &bin, &host.vmm_home)?;
        for vm in vms {
            if vm
                .owner_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(tag_prefix))
            {
                to_remove.push((host.ssh.clone(), host.vmm_home.clone(), bin.clone(), vm));
            }
        }
        host_bins.insert(hkey.clone(), bin);
    }
    Ok((host_bins, to_remove))
}

async fn remove_vms_in_parallel(
    to_remove: &[TaggedVmTarget],
    force: bool,
    timeout_secs: u64,
    exec: &Arc<dyn HostExec>,
) -> Result<Vec<String>> {
    let mut tasks = JoinSet::new();
    for (ssh, vmm_home, bin, vm) in to_remove.iter().cloned() {
        let exec = Arc::clone(exec);
        tasks.spawn_blocking(move || {
            let mut inner = format!(
                "vm remove --name {} --timeout {} --vmm-home {}",
                shell_quote(&vm.name),
                timeout_secs,
                shell_quote_tilde_path(&vmm_home),
            );
            // Pass `--expect-vm-id` whenever the VM has a recorded UUID; omit for `creating` with
            // empty `vm_id` so `outbe-up` can run token-based recovery cleanly.
            if !vm.vm_id.is_empty() {
                inner.push_str(&format!(" --expect-vm-id {}", shell_quote(&vm.vm_id)));
            }
            if force {
                inner.push_str(" --force");
            }
            inner.push_str(" --json");
            let cmd = bin.cmd(&inner);
            let res: Result<RemoteVmRemoveOutput> =
                run_host_json(exec.as_ref(), &ssh, &vm.name, &cmd).wrap_err_with(|| {
                    format!("remove VM `{}` ({}) on {}", vm.name, vm.vm_id, ssh.host)
                });
            (vm.name, res)
        });
    }

    let mut remove_errors = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        let (vm_name, res) = joined.wrap_err("VM remove task panicked")?;
        match res {
            Ok(out) => {
                if let Some(warning) = out.warning {
                    eprintln!("{warning}");
                }
            }
            Err(err) => remove_errors.push(format!("{vm_name}: {err:#}")),
        }
    }
    Ok(remove_errors)
}

fn clean_staging_on_hosts(
    exec: &dyn HostExec,
    desired_members: &[DesiredMember],
    unique_hosts: &BTreeMap<String, UniqueHost>,
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    remove_errors: &mut Vec<String>,
) {
    for member in desired_members {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let cmd = host_bins[&hkey].cmd(&format!(
            "disk clean --staging-name {} --json",
            shell_quote(&member.vm_name)
        ));
        if let Err(err) = run_host_json::<dyn HostExec, RemoteCleanStagingOutput>(
            exec,
            &member.vmm_host,
            &member.name,
            &cmd,
        ) {
            remove_errors.push(format!("disk clean {}: {err:#}", member.vm_name));
        }
    }
    for (hkey, host) in unique_hosts {
        let cmd = host_bins[hkey].cmd("disk clean --stale --json");
        if let Err(err) = run_host_json::<dyn HostExec, RemoteCleanStaleOutput>(
            exec,
            &host.ssh,
            &host.ssh.host,
            &cmd,
        ) {
            remove_errors.push(format!("disk clean --stale on {}: {err:#}", host.ssh.host));
        }
    }
}

fn verify_zero_remaining_resources(
    exec: &dyn HostExec,
    build: &NetworkArtifacts,
    desired_members: &[DesiredMember],
    unique_hosts: &BTreeMap<String, UniqueHost>,
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
) -> Result<()> {
    let tag_prefix =
        network_owner_tag_prefix(&build.config.name, &build.manifest.genesis.final_sha256);
    let member_vm_names: BTreeSet<&str> =
        desired_members.iter().map(|m| m.vm_name.as_str()).collect();
    let mut leftovers = Vec::new();
    for (hkey, host) in unique_hosts {
        let bin = &host_bins[hkey];
        for vm in fetch_host_vms(exec, &host.ssh, bin, &host.vmm_home)? {
            if vm
                .owner_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(&tag_prefix))
            {
                leftovers.push(format!(
                    "VM `{}` ({}) still present on {}",
                    vm.name, vm.vm_id, host.ssh.host
                ));
            }
        }
        for staged in fetch_host_staged_disks(exec, &host.ssh, bin)? {
            if member_vm_names.contains(staged.staging_name.as_str()) {
                leftovers.push(format!(
                    "staged disk `{}` still present on {}",
                    staged.staging_name, host.ssh.host
                ));
            }
        }
    }
    ensure!(
        leftovers.is_empty(),
        "refusing to delete {}: host verification still found remaining network resources: {}",
        NetworkVmState::path(&build.root).display(),
        leftovers.join("; ")
    );
    Ok(())
}
