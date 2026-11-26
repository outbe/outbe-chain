//! Multi-host VM founding-network deploy orchestration (`outbe-net-up network deploy`).

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use eyre::{ensure, Context, Result};
use outbe_operator_node_role::progress::Progress;
use outbe_operator_remote::{run_host_json, shell_quote, HostExec, Ssh};
use outbe_software_bundle::sha256_file;

use super::{
    ensure_vm_deployment,
    host_check::{format_failed_host_checks, run_host_checks, PublishContext},
    host_key,
    images::{publish_image_on_host, resolve_image_source, HostImageTarget, ResolvedImageSource},
    remote_bin::{
        build_founder_tar_bytes, cleanup_remote_dir, ensure_pinned_outbe_up_on_host,
        fetch_host_staged_disks, fetch_host_vms, make_remote_temp_dir,
        resolve_bundled_outbe_up_sha256, RemoteOutbeUp,
    },
    rpc::{ensure_vm_rpc_reachable_for_orchestration, verify_certified_rpc_chain_id},
    shell_quote_tilde_path,
    types::{
        build_desired_members, build_member_state, can_reuse_staged_disk, classify_host_vm,
        collect_unique_hosts, network_owner_tag_prefix, DesiredMember, RemoteCleanStagingOutput,
        RemoteCleanStaleOutput, RemoteDiskPrepareOutput, RemoteHostStatusOutput,
        RemoteImagePublishOutput, RemoteStagedDiskInfo, RemoteVmCreateOutput, RemoteVmStatusOutput,
        UniqueHost, VmClassification,
    },
    DeployOptions, ExitCodeError, EXIT_CODE_HOST_CONFIG_REQUIRED,
};
use crate::network::{
    config::VmFlavor,
    remote::NetworkArtifacts,
    vm_state::{MemberRole, NetworkVmState, NetworkVmStateLock},
};

type HostVmMap = BTreeMap<String, Vec<RemoteVmStatusOutput>>;
type HostStagedMap = BTreeMap<String, BTreeMap<String, RemoteStagedDiskInfo>>;

struct DeployProvisionCtx<'a> {
    build: &'a NetworkArtifacts,
    artifacts: &'a Path,
    state_lock: &'a NetworkVmStateLock,
    exec: &'a dyn HostExec,
    host_bins: &'a BTreeMap<String, RemoteOutbeUp>,
    host_images: &'a BTreeMap<String, RemoteImagePublishOutput>,
    host_staged: &'a HostStagedMap,
    uploaded_op_bundles: &'a BTreeMap<String, (Ssh, String, String)>,
    image_name: &'a str,
    image_hash: &'a str,
}

pub(crate) async fn deploy_with_exec(
    options: &DeployOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("verifying VM network artifacts, bundles, and image source");
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;
    ensure_vm_rpc_reachable_for_orchestration(&build)?;
    let local_op_bundle = verify_local_operator_bundle(&build)?;

    let outbe_up_sha256 = resolve_bundled_outbe_up_sha256(&build.root)?;
    let (resolved_source, image_source_id) = resolve_image_source(&options.image_source)?;

    let state_lock = NetworkVmState::lock(options.artifacts)?;
    let mut state = load_or_init_deploy_state(
        &state_lock,
        options.artifacts,
        &build,
        &image_source_id,
        &outbe_up_sha256,
    )?;

    let mut desired_members =
        build_desired_members(&build, Some(&state), Some(options.no_tee), None, None)?;
    let unique_hosts = collect_unique_hosts(&desired_members);

    let host_bins = prepare_hosts_for_deploy(
        &mut progress,
        exec.as_ref(),
        &build.root,
        &outbe_up_sha256,
        &unique_hosts,
    )?;
    let (host_images, image_name, image_hash) = distribute_guest_image(
        &mut progress,
        exec.as_ref(),
        &unique_hosts,
        &host_bins,
        &resolved_source,
    )?;

    for m in &mut desired_members {
        m.image_name = Some(image_name.clone());
        m.image_hash = Some(image_hash.clone());
    }
    state.image_source_id = image_source_id;
    state.image_name = image_name.clone();
    state.image_hash = image_hash.clone();

    progress.stage("reconciling host VM and staging state");
    let (host_vms, host_staged) = fetch_host_state(exec.as_ref(), &unique_hosts, &host_bins)?;
    verify_certified_rpc_if_running(
        &mut progress,
        &build,
        &desired_members[0].vmm_host,
        &host_vms,
        exec.as_ref(),
    )
    .await?;

    let plans = reconcile_deploy_plans(&build, &state, &desired_members, &unique_hosts, &host_vms)?;
    let hosts_needing_op_bundle = collect_hosts_needing_op_bundle(&plans, &host_staged)?;

    let mut uploaded_op_bundles: BTreeMap<String, (Ssh, String, String)> = BTreeMap::new();
    let deploy_body: Result<()> = (|| {
        upload_op_bundles(
            exec.as_ref(),
            &unique_hosts,
            &hosts_needing_op_bundle,
            &local_op_bundle,
            &mut uploaded_op_bundles,
        )?;
        let ctx = DeployProvisionCtx {
            build: &build,
            artifacts: options.artifacts,
            state_lock: &state_lock,
            exec: exec.as_ref(),
            host_bins: &host_bins,
            host_images: &host_images,
            host_staged: &host_staged,
            uploaded_op_bundles: &uploaded_op_bundles,
            image_name: &image_name,
            image_hash: &image_hash,
        };
        for (member, class) in &plans {
            ctx.provision_member(&mut progress, &mut state, member, class)?;
        }
        Ok(())
    })();

    for (ssh, remote_dir, _) in uploaded_op_bundles.values() {
        let _ = cleanup_remote_dir(exec.as_ref(), ssh, remote_dir);
    }
    deploy_body?;

    run_deploy_host_check_gate(
        &mut progress,
        options.artifacts,
        &desired_members,
        &host_bins,
        exec.as_ref(),
    )
}

fn verify_local_operator_bundle(build: &NetworkArtifacts) -> Result<PathBuf> {
    let local_op_bundle = build.root.join(&build.manifest.joining.bundle);
    ensure!(
        local_op_bundle.is_file(),
        "operator bundle missing at {}",
        local_op_bundle.display()
    );
    ensure!(
        sha256_file(&local_op_bundle)? == build.manifest.joining.bundle_sha256,
        "operator bundle changed after network build: {}",
        local_op_bundle.display()
    );
    Ok(local_op_bundle)
}

fn load_or_init_deploy_state(
    state_lock: &NetworkVmStateLock,
    artifacts: &Path,
    build: &NetworkArtifacts,
    image_source_id: &str,
    outbe_up_sha256: &str,
) -> Result<NetworkVmState> {
    let mut state = match NetworkVmState::load_optional_locked(state_lock, artifacts)? {
        Some(existing) => {
            existing.ensure_matches_network(build)?;
            existing
        }
        None => NetworkVmState::new(
            build,
            image_source_id.to_owned(),
            outbe_up_sha256.to_owned(),
        ),
    };
    state.outbe_up_sha256 = outbe_up_sha256.to_owned();
    Ok(state)
}

fn prepare_hosts_for_deploy(
    progress: &mut Progress,
    exec: &dyn HostExec,
    artifacts_root: &Path,
    outbe_up_sha256: &str,
    unique_hosts: &BTreeMap<String, UniqueHost>,
) -> Result<BTreeMap<String, RemoteOutbeUp>> {
    progress.stage(&format!(
        "pinning outbe-up ({}), checking host readiness, and cleaning stale staging on {} host(s)",
        &outbe_up_sha256[..16],
        unique_hosts.len()
    ));
    let mut host_bins = BTreeMap::new();
    for (hkey, host) in unique_hosts {
        let bin =
            ensure_pinned_outbe_up_on_host(exec, &host.ssh, artifacts_root, outbe_up_sha256, true)?;
        let status_cmd = bin.cmd(&format!(
            "host status --bridge {} --vmm-home {} --json",
            shell_quote(&host.bridge),
            shell_quote_tilde_path(&host.vmm_home),
        ));
        let host_status: RemoteHostStatusOutput =
            run_host_json(exec, &host.ssh, &host.ssh.host, &status_cmd).wrap_err_with(|| {
                format!(
                    "inspect host readiness via `outbe-up host status` on {}",
                    host.ssh.host
                )
            })?;
        ensure!(
            host_status.ready_for_vm,
            "VMM host {} is not ready for VMs (`outbe-up host status` reported ready_for_vm=false): {}",
            host.ssh.host,
            if host_status.notes.is_empty() {
                format!(
                    "run `outbe-up host status --bridge {}` on {} for details",
                    host.bridge, host.ssh.host
                )
            } else {
                host_status.notes.join("; ")
            }
        );

        let stale_cmd = bin.cmd("disk clean --stale --json");
        let stale: RemoteCleanStaleOutput =
            run_host_json(exec, &host.ssh, &host.ssh.host, &stale_cmd)
                .wrap_err_with(|| format!("clean stale staging dirs on {}", host.ssh.host))?;
        if !stale.removed.is_empty() {
            progress.update(&format!(
                "cleaned {} stale staging dir(s) on {}",
                stale.removed.len(),
                host.ssh.host
            ));
        }
        host_bins.insert(hkey.clone(), bin);
    }
    Ok(host_bins)
}

fn distribute_guest_image(
    progress: &mut Progress,
    exec: &dyn HostExec,
    unique_hosts: &BTreeMap<String, UniqueHost>,
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    resolved_source: &ResolvedImageSource,
) -> Result<(BTreeMap<String, RemoteImagePublishOutput>, String, String)> {
    progress.stage(&format!(
        "ensuring guest OS image on {} host(s)",
        unique_hosts.len()
    ));
    let mut host_images = BTreeMap::new();
    let mut uniform_image: Option<(String, String)> = None;
    for (hkey, host) in unique_hosts {
        let target = HostImageTarget {
            ssh: &host.ssh,
            bin: &host_bins[hkey],
            vmm_home: &host.vmm_home,
        };
        let published = publish_image_on_host(exec, &target, resolved_source)?;
        match &uniform_image {
            None => {
                uniform_image = Some((
                    published.image_name.clone(),
                    published.os_image_hash.clone(),
                ));
            }
            Some((img_name, img_hash)) => {
                ensure!(
                    *img_name == published.image_name && *img_hash == published.os_image_hash,
                    "image digest mismatch across hosts: host {} reported `{}` ({}), expected `{img_name}` ({img_hash})",
                    host.ssh.host,
                    published.image_name,
                    published.os_image_hash
                );
            }
        }
        host_images.insert(hkey.clone(), published);
    }
    let (image_name, image_hash) =
        uniform_image.ok_or_else(|| eyre::eyre!("no target hosts found for VM deployment"))?;
    Ok((host_images, image_name, image_hash))
}

fn fetch_host_state(
    exec: &dyn HostExec,
    unique_hosts: &BTreeMap<String, UniqueHost>,
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
) -> Result<(HostVmMap, HostStagedMap)> {
    let mut host_vms = BTreeMap::new();
    let mut host_staged = BTreeMap::new();
    for (hkey, host) in unique_hosts {
        let bin = &host_bins[hkey];
        let vms = fetch_host_vms(exec, &host.ssh, bin, &host.vmm_home)?;
        let staged = fetch_host_staged_disks(exec, &host.ssh, bin)?;
        let map = staged
            .into_iter()
            .map(|item| (item.staging_name.clone(), item))
            .collect();
        host_vms.insert(hkey.clone(), vms);
        host_staged.insert(hkey.clone(), map);
    }
    Ok((host_vms, host_staged))
}

async fn verify_certified_rpc_if_running(
    progress: &mut Progress,
    build: &NetworkArtifacts,
    first_host: &Ssh,
    host_vms: &HostVmMap,
    exec: &dyn HostExec,
) -> Result<()> {
    let tag_prefix =
        network_owner_tag_prefix(&build.config.name, &build.manifest.genesis.final_sha256);
    let any_network_vm_running = host_vms.values().flatten().any(|vm| {
        vm.running
            && vm
                .owner_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(&tag_prefix))
    });
    if any_network_vm_running {
        progress.stage(&format!(
            "verifying certified_rpc ({}) answers eth_chainId == {}",
            build.config.certified_rpc, build.config.chain_id
        ));
        verify_certified_rpc_chain_id(build, first_host, exec).await?;
    }
    Ok(())
}

fn reconcile_deploy_plans<'a>(
    build: &NetworkArtifacts,
    state: &NetworkVmState,
    desired_members: &'a [DesiredMember],
    unique_hosts: &BTreeMap<String, UniqueHost>,
    host_vms: &HostVmMap,
) -> Result<Vec<(&'a DesiredMember, VmClassification)>> {
    let mut expected_names: BTreeSet<&str> =
        desired_members.iter().map(|m| m.vm_name.as_str()).collect();
    for extra in state
        .members
        .iter()
        .filter(|m| m.role != MemberRole::Founder)
    {
        expected_names.insert(extra.vm_name.as_str());
    }

    let mut blockers = Vec::new();
    let mut plans = Vec::with_capacity(desired_members.len());
    for member in desired_members {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let observed = host_vms[&hkey].iter().find(|vm| vm.name == member.vm_name);
        let class = classify_host_vm(member, observed);
        if let Some(blocker) = deploy_blocker_for_member(member, &class, &build.root) {
            blockers.push(blocker);
        }
        plans.push((member, class));
    }

    blockers.extend(collect_orphan_deploy_blockers(
        build,
        unique_hosts,
        host_vms,
        &expected_names,
    ));

    ensure!(
        blockers.is_empty(),
        "refusing to deploy due to host VM conflicts:\n  - {}",
        blockers.join("\n  - ")
    );
    Ok(plans)
}

fn deploy_blocker_for_member(
    member: &DesiredMember,
    class: &VmClassification,
    artifacts_root: &Path,
) -> Option<String> {
    match class {
        VmClassification::Foreign { observed, reason } => Some(format!(
            "member `{}` (`{}` on {}, vm_id `{}`): FOREIGN ({reason}) — not ours: check the owner, and use another network name or VM name.",
            member.name, member.vm_name, member.vmm_host.host, observed.vm_id
        )),
        VmClassification::Stale { observed, reasons } => {
            let rm_hint = if observed.vm_id.is_empty() {
                format!(
                    "outbe-up vm remove --name {} --forget-intent",
                    observed.name
                )
            } else {
                format!(
                    "outbe-up vm remove --name {} --expect-vm-id {}",
                    observed.name, observed.vm_id
                )
            };
            Some(format!(
                "member `{}` (`{}` on {}): STALE ({}). Remove and redeploy: `{rm_hint}` (or `outbe-net-up network remove --artifacts {}`)",
                member.name,
                member.vm_name,
                member.vmm_host.host,
                reasons.join(", "),
                artifacts_root.display()
            ))
        }
        VmClassification::Partial { observed } if observed.phase == "removing" => Some(format!(
            "member `{}` (`{}` on {}): PARTIAL (mid-removal). Finish removal with `outbe-up vm remove --name {}` before redeploying.",
            member.name, member.vm_name, member.vmm_host.host, observed.name
        )),
        VmClassification::Absent
        | VmClassification::Partial { .. }
        | VmClassification::Ours { .. } => None,
    }
}

fn collect_orphan_deploy_blockers(
    build: &NetworkArtifacts,
    unique_hosts: &BTreeMap<String, UniqueHost>,
    host_vms: &HostVmMap,
    expected_names: &BTreeSet<&str>,
) -> Vec<String> {
    let tag_prefix =
        network_owner_tag_prefix(&build.config.name, &build.manifest.genesis.final_sha256);
    let mut blockers = Vec::new();
    for (hkey, vms) in host_vms {
        let host = &unique_hosts[hkey];
        for vm in vms {
            let is_orphan = vm
                .owner_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(&tag_prefix))
                && !expected_names.contains(vm.name.as_str());
            if is_orphan {
                blockers.push(format!(
                    "host {} has ORPHAN VM `{}` ({}, tag `{}`). Remove it with `outbe-up vm remove --name {} --expect-vm-id {}` (or `outbe-net-up network remove --artifacts {}`).",
                    host.ssh.host,
                    vm.name,
                    vm.vm_id,
                    vm.owner_tag.as_deref().unwrap_or(""),
                    vm.name,
                    vm.vm_id,
                    build.root.display()
                ));
            }
        }
    }
    blockers
}

fn collect_hosts_needing_op_bundle(
    plans: &[(&DesiredMember, VmClassification)],
    host_staged: &HostStagedMap,
) -> Result<BTreeSet<String>> {
    let mut hosts = BTreeSet::new();
    for (member, class) in plans {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let staged_entry = host_staged[&hkey].get(&member.vm_name);
        match class {
            VmClassification::Ours { .. } => {}
            VmClassification::Absent => {
                if !can_reuse_staged_disk(staged_entry, member.recorded_disk_sha256.as_deref()) {
                    hosts.insert(hkey);
                }
            }
            VmClassification::Partial { observed } => {
                let required_sha = observed
                    .disk_source_sha256
                    .as_deref()
                    .or(member.recorded_disk_sha256.as_deref());
                if !can_reuse_staged_disk(staged_entry, required_sha) {
                    ensure!(
                        member.role == MemberRole::Founder || required_sha.is_none(),
                        "member `{}` (`{}` on {}) is in partial phase `{}` requiring staged disk sha256 `{}`, which is no longer present on host; remove the partial VM with `outbe-up vm remove --name {}` and re-run deploy",
                        member.name,
                        member.vm_name,
                        member.vmm_host.host,
                        observed.phase,
                        required_sha.unwrap_or(""),
                        member.vm_name
                    );
                    hosts.insert(hkey);
                }
            }
            VmClassification::Foreign { .. } | VmClassification::Stale { .. } => {
                unreachable!("blocked above")
            }
        }
    }
    Ok(hosts)
}

fn upload_op_bundles(
    exec: &dyn HostExec,
    unique_hosts: &BTreeMap<String, UniqueHost>,
    hosts_needing_op_bundle: &BTreeSet<String>,
    local_op_bundle: &Path,
    uploaded_op_bundles: &mut BTreeMap<String, (Ssh, String, String)>,
) -> Result<()> {
    let op_bundle_filename = local_op_bundle
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("outbe-validator-operator-bundle.tar.zst");
    for hkey in hosts_needing_op_bundle {
        let host = &unique_hosts[hkey];
        let remote_dir = make_remote_temp_dir(exec, &host.ssh, "outbe-net-up-opbundle")?;
        exec.copy_to(
            &host.ssh,
            &host.ssh.host,
            &[local_op_bundle],
            &remote_dir,
            false,
        )
        .wrap_err_with(|| format!("upload operator bundle to {}", host.ssh.host))?;
        let remote_bundle = format!("{remote_dir}/{op_bundle_filename}");
        uploaded_op_bundles.insert(hkey.clone(), (host.ssh.clone(), remote_dir, remote_bundle));
    }
    Ok(())
}

impl DeployProvisionCtx<'_> {
    fn provision_member(
        &self,
        progress: &mut Progress,
        state: &mut NetworkVmState,
        member: &DesiredMember,
        class: &VmClassification,
    ) -> Result<()> {
        if let VmClassification::Ours { observed } = class {
            progress.update(&format!(
                "reconciled `{}` ({}) on {}: already up-to-date",
                member.vm_name, observed.vm_id, member.vmm_host.host
            ));
            state.upsert_member(build_member_state(
                member,
                self.image_hash,
                observed
                    .disk_source_sha256
                    .clone()
                    .or_else(|| member.recorded_disk_sha256.clone()),
                Some(observed.vm_id.clone()),
                Some(observed.mac.clone()),
            ));
            state.save_locked(self.state_lock, self.artifacts)?;
            return Ok(());
        }

        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let bin = &self.host_bins[&hkey];
        let host_img = &self.host_images[&hkey];
        let partial_required_sha = match class {
            VmClassification::Partial { observed } => observed.disk_source_sha256.as_deref(),
            _ => None,
        };
        let disk_sha256 =
            self.ensure_founder_staged_disk(progress, state, member, partial_required_sha)?;

        let created = create_deploy_vm(progress, self, bin, host_img, member)?;
        state.upsert_member(build_member_state(
            member,
            self.image_hash,
            Some(disk_sha256),
            Some(created.vm_id),
            Some(created.mac),
        ));
        state.save_locked(self.state_lock, self.artifacts)?;
        Ok(())
    }

    fn ensure_founder_staged_disk(
        &self,
        progress: &mut Progress,
        state: &mut NetworkVmState,
        member: &DesiredMember,
        partial_required_sha: Option<&str>,
    ) -> Result<String> {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let reusable_sha = partial_required_sha.or(member.recorded_disk_sha256.as_deref());
        if can_reuse_staged_disk(self.host_staged[&hkey].get(&member.vm_name), reusable_sha) {
            let sha = reusable_sha.expect("reusable_sha checked").to_owned();
            progress.update(&format!(
                "reusing staged disk `{}` (sha256 {}...) on {}",
                member.vm_name,
                &sha[..12],
                member.vmm_host.host
            ));
            return Ok(sha);
        }

        progress.stage(&format!(
            "preparing founder data disk for {} on {} via stdin tar",
            member.name, member.vmm_host.host
        ));
        let local_tree = self
            .build
            .root
            .join(format!("vm/validator-{}", member.index));
        let tar_bytes = build_founder_tar_bytes(&local_tree)?;
        let (_, _, remote_op_bundle) = &self.uploaded_op_bundles[&hkey];
        let prep_cmd = self.host_bins[&hkey].cmd(&format!(
            "disk prepare --role founder --source - --bundle {} --staging-name {} --size-gib {} --json",
            shell_quote(remote_op_bundle),
            shell_quote(&member.vm_name),
            member.disk_gib,
        ));
        let raw = self
            .exec
            .capture_stdin(&member.vmm_host, &member.name, &prep_cmd, &tar_bytes)
            .wrap_err_with(|| {
                format!(
                    "stream founder data disk for `{}` on {}",
                    member.vm_name, member.vmm_host.host
                )
            })?;
        let prep: RemoteDiskPrepareOutput = serde_json::from_str(&raw).wrap_err_with(|| {
            format!(
                "parse `outbe-up disk prepare --json` output for `{}`",
                member.vm_name
            )
        })?;
        if let Some(expected_sha) = partial_required_sha {
            ensure!(
                prep.sha256 == expected_sha,
                "re-prepared staged disk for partial VM `{}` produced sha256 `{}`, which differs from recorded `{expected_sha}`; remove the partial VM with `outbe-up vm remove --name {}` and re-run deploy",
                member.vm_name,
                prep.sha256,
                member.vm_name
            );
        }
        state.upsert_member(build_member_state(
            member,
            self.image_hash,
            Some(prep.sha256.clone()),
            state.member(&member.vm_name).and_then(|m| m.vm_id.clone()),
            state.member(&member.vm_name).and_then(|m| m.mac.clone()),
        ));
        state.save_locked(self.state_lock, self.artifacts)?;
        Ok(prep.sha256)
    }
}

fn create_deploy_vm(
    progress: &mut Progress,
    ctx: &DeployProvisionCtx<'_>,
    bin: &RemoteOutbeUp,
    host_img: &RemoteImagePublishOutput,
    member: &DesiredMember,
) -> Result<RemoteVmCreateOutput> {
    progress.stage(&format!(
        "creating stopped VM `{}` on {}",
        member.vm_name, member.vmm_host.host
    ));
    let flavor_arg = match member.flavor {
        VmFlavor::Dev => "dev",
        VmFlavor::Prod => "prod",
    };
    let mut create_inner = format!(
        "vm create --name {} --owner-tag {} --image {} --image-name {} --flavor {} --disk-staging {} --consume-staging --bridge {} --ip {} --vcpu {} --memory-gib {} --disk-gib {} --vmm-home {}",
        shell_quote(&member.vm_name),
        shell_quote(&member.owner_tag),
        shell_quote(&host_img.target_dir),
        shell_quote(&host_img.image_name),
        flavor_arg,
        shell_quote(&member.vm_name),
        shell_quote(&member.bridge),
        member.service_ip,
        member.vcpu,
        member.memory_gib,
        member.disk_gib,
        shell_quote_tilde_path(&member.vmm_home),
    );
    if member.no_tee {
        create_inner.push_str(" --no-tee");
    }
    create_inner.push_str(" --json");
    let create_cmd = bin.cmd(&create_inner);
    let created: RemoteVmCreateOutput =
        run_host_json(ctx.exec, &member.vmm_host, &member.name, &create_cmd).wrap_err_with(
            || format!("create VM `{}` on {}", member.vm_name, member.vmm_host.host),
        )?;
    ensure!(
        created.name == member.vm_name
            && created.image_name == ctx.image_name
            && created.os_image_hash == ctx.image_hash,
        "unexpected `vm create` result for `{}` on {}",
        member.vm_name,
        member.vmm_host.host
    );
    if let Some(warning) = &created.warning {
        eprintln!("{warning}");
    }
    if !created.staging_consumed {
        let clean_cmd = bin.cmd(&format!(
            "disk clean --staging-name {} --json",
            shell_quote(&member.vm_name)
        ));
        let _ = run_host_json::<dyn HostExec, RemoteCleanStagingOutput>(
            ctx.exec,
            &member.vmm_host,
            &member.name,
            &clean_cmd,
        );
    }
    Ok(created)
}

fn run_deploy_host_check_gate(
    progress: &mut Progress,
    artifacts: &Path,
    desired_members: &[DesiredMember],
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    exec: &dyn HostExec,
) -> Result<()> {
    progress.stage("verifying host networking and published ports (`outbe-up host check`)");
    let publish_ctx = PublishContext::from_members(desired_members);
    let checks = run_host_checks(desired_members, host_bins, exec, &publish_ctx)?;
    if checks.iter().all(|(_, c)| c.ready) {
        progress.done(&format!(
            "deployed {} stopped VM(s); host checks passed",
            desired_members.len()
        ));
        println!(
            "Next: outbe-net-up network start --artifacts {}",
            artifacts.display()
        );
        return Ok(());
    }

    let report_text = format_failed_host_checks(&checks);
    println!(
        "\nHost configuration required before starting the network (read-only; zero writes performed):\n{report_text}\n\nApply the host configuration above, then run:\n  outbe-net-up network start --artifacts {}",
        artifacts.display()
    );
    Err(eyre::Report::new(ExitCodeError {
        code: EXIT_CODE_HOST_CONFIG_REQUIRED,
        message: format!(
            "host check failed on one or more hosts; apply the host configuration printed above, then run `outbe-net-up network start --artifacts {}`",
            artifacts.display()
        ),
    }))
}
