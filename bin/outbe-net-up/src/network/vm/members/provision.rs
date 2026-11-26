//! Shared member VM provisioning (validation, reconcile, staged disk prepare, vm create, host check).

use std::{
    collections::BTreeMap,
    fs,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use alloy_primitives::Address;
use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::{
    progress::Progress,
    vm_network::{StaticVmNetworkConfig, VmPortsConfig},
};
use outbe_operator_remote::{run_host_json, shell_quote, validate_ssh, HostExec, Ssh};
use outbe_software_bundle::sha256_file;

use super::super::{
    build_all_desired_members, build_host_check_command, build_member_state, can_reuse_staged_disk,
    classify_host_vm, cleanup_remote_dir, ensure_pinned_outbe_up_on_host, ensure_vm_deployment,
    ensure_vm_rpc_reachable_for_orchestration, fetch_host_staged_disks, fetch_host_vms,
    format_failed_host_checks, founder_vm_name, host_key, make_remote_temp_dir, member_owner_tag,
    parse_host_check_output, publish_image_on_host, resolve_image_source,
    resolve_state_or_bundled_outbe_up_sha256, shell_quote_tilde_path, DesiredMember, ExitCodeError,
    HostImageTarget, ImageSource, PublishContext, RemoteCleanStagingOutput, RemoteCleanStaleOutput,
    RemoteDiskPrepareOutput, RemoteHostStatusOutput, RemoteImagePublishOutput, RemoteListedImage,
    RemoteOutbeUp, RemoteStagedDiskInfo, RemoteVmCreateOutput, RemoteVmStartOutput,
    VmClassification, EXIT_CODE_HOST_CONFIG_REQUIRED,
};
use super::keys::{build_full_node_config, build_joining_validator_tar_bytes};
use crate::network::{
    config::{FounderPublishedPorts, VmFlavor},
    remote::NetworkArtifacts,
    vm_state::{MemberRole, NetworkVmState, NetworkVmStateLock},
};

pub(super) struct ProvisionRequest<'a> {
    pub(super) artifacts: &'a Path,
    pub(super) role: MemberRole,
    pub(super) name: &'a str,
    pub(super) host: &'a str,
    pub(super) user: Option<&'a str>,
    pub(super) port: u16,
    pub(super) vmm_home: Option<&'a str>,
    pub(super) bridge: Option<&'a str>,
    pub(super) vm_ip: Ipv4Addr,
    pub(super) rpc_bind_ip: Option<Ipv4Addr>,
    pub(super) advertise_ip: Ipv4Addr,
    pub(super) rpc_port: u16,
    pub(super) port_offset: u16,
    pub(super) image_source: Option<&'a ImageSource>,
    pub(super) vcpu: Option<u32>,
    pub(super) memory_gib: Option<u32>,
    pub(super) disk_gib: Option<u32>,
    pub(super) no_tee: bool,
}

pub(super) enum MemberDiskSource<'a> {
    /// Joining validator: local keys directory + validator EVM address + published ports.
    /// Built in a private tempdir and streamed over stdin tar to `disk prepare --source -`.
    JoiningValidator {
        local_keys_dir: &'a Path,
        validator_addr: Address,
        ports: FounderPublishedPorts,
    },
    /// Keyless full node: zero provisioned keys locally, in transit, or on disk (Reth generates
    /// its own ephemeral P2P key inside `--datadir`).
    FullNodeKeyless,
}

pub(super) struct ProvisionedMember {
    pub(super) build: NetworkArtifacts,
    pub(super) member: DesiredMember,
    pub(super) all_members: Vec<DesiredMember>,
    pub(super) target_hkey: String,
    pub(super) target_bin: RemoteOutbeUp,
    pub(super) outbe_up_sha256: String,
    pub(super) vm_id: String,
}

struct NewMemberSpec<'a> {
    role: MemberRole,
    name: &'a str,
    vmm_host: Ssh,
    vmm_home: String,
    bridge: String,
    service_ip: Ipv4Addr,
    advertise_ip: Ipv4Addr,
    rpc_port: u16,
    port_offset: u16,
    vcpu: u32,
    memory_gib: u32,
    disk_gib: u32,
    flavor: VmFlavor,
    no_tee: bool,
}

struct StagedDiskContext<'a> {
    build: &'a NetworkArtifacts,
    member: &'a DesiredMember,
    local_op_bundle: &'a Path,
    target_bin: &'a RemoteOutbeUp,
    exec: &'a dyn HostExec,
}

struct MemberReconcileContext<'a> {
    build: &'a NetworkArtifacts,
    artifacts: &'a Path,
    role_label: &'a str,
    local_op_bundle: &'a Path,
    target_bin: &'a RemoteOutbeUp,
    host_img: &'a RemoteImagePublishOutput,
    disk_source: &'a MemberDiskSource<'a>,
    exec: &'a dyn HostExec,
    state_lock: &'a NetworkVmStateLock,
}

fn role_label(role: MemberRole) -> &'static str {
    match role {
        MemberRole::JoiningValidator => "joining-validator",
        MemberRole::FullNode => "keyless full-node",
        MemberRole::Founder => "founder",
    }
}

pub(super) fn provision_member(
    req: &ProvisionRequest<'_>,
    disk_source: &MemberDiskSource<'_>,
    exec: &dyn HostExec,
    progress: &mut Progress,
) -> Result<ProvisionedMember> {
    let label = role_label(req.role);
    progress.stage(&format!(
        "validating {label} `{}` configuration and network state",
        req.name
    ));

    let (build, local_op_bundle) = load_and_check_provision_artifacts(req)?;
    let vm_defaults = build
        .config
        .vm
        .as_ref()
        .ok_or_else(|| eyre::eyre!("network config has no vm section"))?;

    let state_lock = NetworkVmState::lock(req.artifacts)?;
    let mut state = NetworkVmState::load_optional_locked(&state_lock, req.artifacts)?
        .ok_or_else(|| {
            eyre::eyre!(
                "network-vm-state.json not found in {}; deploy the founding network before adding a {label}",
                req.artifacts.display()
            )
        })?;
    state.ensure_matches_network(&build)?;
    let outbe_up_sha256 = resolve_state_or_bundled_outbe_up_sha256(&build.root, Some(&state))?;

    let existing_members = build_all_desired_members(&build, Some(&state), None, None, None)?;
    let vmm_host = resolve_target_ssh(req.host, req.user, req.port, vm_defaults.vmm_host.as_ref())?;
    let vmm_home = req.vmm_home.unwrap_or(&vm_defaults.vmm_home).to_owned();
    let bridge = req.bridge.unwrap_or(&vm_defaults.bridge).to_owned();

    let mut member = build_and_validate_new_member(
        &build,
        &state,
        &existing_members,
        NewMemberSpec {
            role: req.role,
            name: req.name,
            vmm_host,
            vmm_home,
            bridge,
            service_ip: req.vm_ip,
            advertise_ip: req.advertise_ip,
            rpc_port: req.rpc_port,
            port_offset: req.port_offset,
            vcpu: req.vcpu.unwrap_or(vm_defaults.vcpu),
            memory_gib: req.memory_gib.unwrap_or(vm_defaults.memory_gib),
            disk_gib: req.disk_gib.unwrap_or(vm_defaults.disk_gib),
            flavor: vm_defaults.flavor,
            no_tee: req.no_tee,
        },
    )?;

    progress.stage(&format!(
        "pinning outbe-up, verifying host {}, and resolving guest OS image",
        member.vmm_host.host
    ));
    let target_hkey = host_key(&member.vmm_host, &member.vmm_home);
    let target_bin = ensure_pinned_outbe_up_on_host(
        exec,
        &member.vmm_host,
        &build.root,
        &outbe_up_sha256,
        true,
    )?;
    verify_host_ready_and_clean_stale(
        exec,
        &member.vmm_host,
        &target_bin,
        &member.bridge,
        &member.vmm_home,
    )?;
    let img_target = HostImageTarget {
        ssh: &member.vmm_host,
        bin: &target_bin,
        vmm_home: &member.vmm_home,
    };
    let host_img = resolve_image_on_target_host(exec, &img_target, req.image_source, &state)?;
    member.image_name = Some(host_img.image_name.clone());
    member.image_hash = Some(host_img.os_image_hash.clone());

    let reconcile_ctx = MemberReconcileContext {
        build: &build,
        artifacts: req.artifacts,
        role_label: label,
        local_op_bundle: &local_op_bundle,
        target_bin: &target_bin,
        host_img: &host_img,
        disk_source,
        exec,
        state_lock: &state_lock,
    };
    let vm_id = reconcile_and_create_member(&reconcile_ctx, &member, &mut state, progress)?;

    let mut all_members = existing_members;
    if !all_members.iter().any(|m| m.vm_name == member.vm_name) {
        all_members.push(member.clone());
    }
    verify_member_host_check(exec, &target_bin, &member, &all_members, progress)?;
    drop(state_lock);

    Ok(ProvisionedMember {
        build,
        member,
        all_members,
        target_hkey,
        target_bin,
        outbe_up_sha256,
        vm_id,
    })
}

fn load_and_check_provision_artifacts(
    req: &ProvisionRequest<'_>,
) -> Result<(NetworkArtifacts, PathBuf)> {
    let build = NetworkArtifacts::load(req.artifacts)?;
    ensure_vm_deployment(&build)?;
    ensure_vm_rpc_reachable_for_orchestration(&build)?;
    if let Some(rpc_bind_ip) = req.rpc_bind_ip {
        ensure!(
            rpc_bind_ip == req.vm_ip,
            "refusing VM orchestration: member `{}` sets rpc_bind_ip={rpc_bind_ip} != vm_ip={}; \
             RPC-based readiness checks require rpc_bind_ip to match vm_ip (omit --rpc-bind-ip or set it to {})",
            req.name,
            req.vm_ip,
            req.vm_ip
        );
    }
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
    Ok((build, local_op_bundle))
}

fn reconcile_and_create_member(
    ctx: &MemberReconcileContext<'_>,
    member: &DesiredMember,
    state: &mut NetworkVmState,
    progress: &mut Progress,
) -> Result<String> {
    let host_vms = fetch_host_vms(ctx.exec, &member.vmm_host, ctx.target_bin, &member.vmm_home)?;
    let host_staged: BTreeMap<String, RemoteStagedDiskInfo> =
        fetch_host_staged_disks(ctx.exec, &member.vmm_host, ctx.target_bin)?
            .into_iter()
            .map(|d| (d.staging_name.clone(), d))
            .collect();
    let observed = host_vms.iter().find(|vm| vm.name == member.vm_name);
    let class = classify_host_vm(member, observed);

    match class {
        VmClassification::Foreign { observed, reason } => {
            bail!(
                "refusing to add `{}` (`{}` on {}, vm_id `{}`): FOREIGN ({reason})",
                member.name,
                member.vm_name,
                member.vmm_host.host,
                observed.vm_id
            );
        }
        VmClassification::Stale { observed, reasons } => {
            bail!(
                "refusing to add `{}` (`{}` on {}, vm_id `{}`): STALE ({}); remove the stale VM first",
                member.name,
                member.vm_name,
                member.vmm_host.host,
                observed.vm_id,
                reasons.join(", ")
            );
        }
        VmClassification::Partial { observed } if observed.phase == "removing" => {
            bail!(
                "VM `{}` on {} is mid-removal; finish `outbe-up vm remove --name {}` first",
                member.vm_name,
                member.vmm_host.host,
                member.vm_name
            );
        }
        VmClassification::Ours { observed } => {
            progress.update(&format!(
                "reusing existing {} VM `{}` ({}) on {}",
                ctx.role_label, member.vm_name, observed.vm_id, member.vmm_host.host
            ));
            state.upsert_member(build_member_state(
                member,
                &ctx.host_img.os_image_hash,
                observed
                    .disk_source_sha256
                    .clone()
                    .or_else(|| member.recorded_disk_sha256.clone()),
                Some(observed.vm_id.clone()),
                Some(observed.mac.clone()),
            ));
            state.save_locked(ctx.state_lock, ctx.artifacts)?;
            Ok(observed.vm_id)
        }
        VmClassification::Absent | VmClassification::Partial { .. } => {
            let partial_required_sha = match &class {
                VmClassification::Partial { observed } => observed.disk_source_sha256.as_deref(),
                _ => None,
            };
            let disk_sha256 = ensure_member_staged_disk(
                ctx,
                member,
                (&host_staged, partial_required_sha),
                state,
                progress,
            )?;

            progress.stage(&format!(
                "creating stopped {} VM `{}` on {}",
                ctx.role_label, member.vm_name, member.vmm_host.host
            ));
            let created =
                create_member_vm_from_staging(ctx.exec, ctx.target_bin, member, ctx.host_img)?;
            state.upsert_member(build_member_state(
                member,
                &ctx.host_img.os_image_hash,
                Some(disk_sha256),
                Some(created.vm_id.clone()),
                Some(created.mac),
            ));
            state.save_locked(ctx.state_lock, ctx.artifacts)?;
            Ok(created.vm_id)
        }
    }
}

fn ensure_member_staged_disk(
    ctx: &MemberReconcileContext<'_>,
    member: &DesiredMember,
    (host_staged, partial_required_sha): (&BTreeMap<String, RemoteStagedDiskInfo>, Option<&str>),
    state: &mut NetworkVmState,
    progress: &mut Progress,
) -> Result<String> {
    let reusable_sha = partial_required_sha.or(member.recorded_disk_sha256.as_deref());
    if can_reuse_staged_disk(host_staged.get(&member.vm_name), reusable_sha) {
        return Ok(reusable_sha.expect("checked above").to_owned());
    }
    let disk_ctx = StagedDiskContext {
        build: ctx.build,
        member,
        local_op_bundle: ctx.local_op_bundle,
        target_bin: ctx.target_bin,
        exec: ctx.exec,
    };
    let prep = prepare_member_staged_disk(&disk_ctx, ctx.disk_source, progress)?;
    if let Some(expected_sha) = partial_required_sha {
        ensure!(
            prep.sha256 == expected_sha,
            "re-prepared staged disk for partial VM `{}` produced sha256 `{}`, expected `{expected_sha}`",
            member.vm_name,
            prep.sha256
        );
    }
    state.upsert_member(build_member_state(
        member,
        &ctx.host_img.os_image_hash,
        Some(prep.sha256.clone()),
        state.member(&member.vm_name).and_then(|m| m.vm_id.clone()),
        state.member(&member.vm_name).and_then(|m| m.mac.clone()),
    ));
    state.save_locked(ctx.state_lock, ctx.artifacts)?;
    Ok(prep.sha256)
}

fn verify_member_host_check(
    exec: &dyn HostExec,
    target_bin: &RemoteOutbeUp,
    member: &DesiredMember,
    all_members: &[DesiredMember],
    progress: &mut Progress,
) -> Result<()> {
    let label = role_label(member.role);
    let publish_ctx = PublishContext::from_members(all_members);
    if member.role == MemberRole::FullNode {
        ensure!(
            publish_ctx.p2p_port_specs(member)?.is_empty(),
            "full-node host check must never request inbound published ports"
        );
    }
    progress.stage(&format!(
        "verifying host networking for {label} `{}` (`outbe-up host check`)",
        member.vm_name
    ));
    let check_cmd = build_host_check_command(member, target_bin, &publish_ctx)?;
    let raw_check = exec
        .capture_raw(&member.vmm_host, &member.name, &check_cmd)
        .wrap_err_with(|| {
            format!(
                "run `outbe-up host check` for `{}` on {}",
                member.vm_name, member.vmm_host.host
            )
        })?;
    let (mut report, rc) = parse_host_check_output(&raw_check, &member.vm_name)?;
    if rc != 0 || !report.service_ip_ok || !report.host_network_ok || !report.published_ports_ok {
        report.ready = false;
    }
    if report.ready {
        return Ok(());
    }
    let checks = vec![(member.vmm_host.host.clone(), report)];
    let report_text = format_failed_host_checks(&checks);
    println!(
        "\nHost configuration required before starting `{}` (read-only; zero writes performed):\n{report_text}",
        member.vm_name
    );
    let rerun_cmd = match member.role {
        MemberRole::JoiningValidator => "outbe-net-up network add-validator",
        MemberRole::FullNode => "outbe-net-up network add-full-node",
        MemberRole::Founder => "outbe-net-up network start",
    };
    Err(eyre::Report::new(ExitCodeError {
        code: EXIT_CODE_HOST_CONFIG_REQUIRED,
        message: format!(
            "host check failed for {label} `{}` on {}; apply the host configuration above and re-run `{rerun_cmd}`",
            member.vm_name, member.vmm_host.host
        ),
    }))
}

fn prepare_member_staged_disk(
    ctx: &StagedDiskContext<'_>,
    disk_source: &MemberDiskSource<'_>,
    progress: &mut Progress,
) -> Result<RemoteDiskPrepareOutput> {
    let op_filename = ctx
        .local_op_bundle
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("outbe-validator-operator-bundle.tar.zst");

    match disk_source {
        MemberDiskSource::JoiningValidator {
            local_keys_dir,
            validator_addr,
            ports,
        } => prepare_joining_validator_disk(
            ctx,
            op_filename,
            local_keys_dir,
            (*validator_addr, *ports),
            progress,
        ),
        MemberDiskSource::FullNodeKeyless => prepare_full_node_disk(ctx, op_filename, progress),
    }
}

fn prepare_joining_validator_disk(
    ctx: &StagedDiskContext<'_>,
    op_filename: &str,
    local_keys_dir: &Path,
    (validator_addr, ports): (Address, FounderPublishedPorts),
    progress: &mut Progress,
) -> Result<RemoteDiskPrepareOutput> {
    let member = ctx.member;
    progress.stage(&format!(
        "streaming joining-validator data disk for `{}` to {} via stdin tar",
        member.name, member.vmm_host.host
    ));
    let tar_bytes = build_joining_validator_tar_bytes(
        ctx.build,
        member,
        local_keys_dir,
        validator_addr,
        ports,
    )?;
    let remote_dir = make_remote_temp_dir(ctx.exec, &member.vmm_host, "outbe-net-up-opbundle")?;
    let remote_bundle = format!("{remote_dir}/{op_filename}");
    let prep_res: Result<RemoteDiskPrepareOutput> = (|| {
        ctx.exec
            .copy_to(
                &member.vmm_host,
                &member.vmm_host.host,
                &[ctx.local_op_bundle],
                &remote_dir,
                false,
            )
            .wrap_err_with(|| format!("upload operator bundle to {}", member.vmm_host.host))?;
        let prep_cmd = ctx.target_bin.cmd(&format!(
            "disk prepare --role joining-validator --source - --bundle {} --staging-name {} --size-gib {} --json",
            shell_quote(&remote_bundle),
            shell_quote(&member.vm_name),
            member.disk_gib,
        ));
        let raw = ctx
            .exec
            .capture_stdin(&member.vmm_host, &member.name, &prep_cmd, &tar_bytes)
            .wrap_err_with(|| {
                format!(
                    "stream joining-validator data disk for `{}` on {}",
                    member.vm_name, member.vmm_host.host
                )
            })?;
        serde_json::from_str(&raw).wrap_err_with(|| {
            format!(
                "parse `outbe-up disk prepare --json` output for `{}`",
                member.vm_name
            )
        })
    })();
    let _ = cleanup_remote_dir(ctx.exec, &member.vmm_host, &remote_dir);
    prep_res
}

fn prepare_full_node_disk(
    ctx: &StagedDiskContext<'_>,
    op_filename: &str,
    progress: &mut Progress,
) -> Result<RemoteDiskPrepareOutput> {
    let member = ctx.member;
    progress.stage(&format!(
        "preparing keyless full-node data disk for `{}` on {}",
        member.name, member.vmm_host.host
    ));
    let full_cfg = build_full_node_config(ctx.build, member)?;
    let full_yaml = serde_yaml_ng::to_string(&full_cfg)?;
    let local_tmp = tempfile::Builder::new()
        .prefix("outbe-fullnode-cfg-")
        .tempdir()
        .wrap_err("create temp dir for full-node config")?;
    fs::set_permissions(local_tmp.path(), fs::Permissions::from_mode(0o700))?;
    let local_cfg_path = local_tmp.path().join("full-node.yaml");
    fs::write(&local_cfg_path, full_yaml.as_bytes())?;
    fs::set_permissions(&local_cfg_path, fs::Permissions::from_mode(0o600))?;

    let remote_dir = make_remote_temp_dir(ctx.exec, &member.vmm_host, "outbe-net-up-fullnode")?;
    let remote_bundle = format!("{remote_dir}/{op_filename}");
    let remote_cfg = format!("{remote_dir}/full-node.yaml");
    let prep_res: Result<RemoteDiskPrepareOutput> = (|| {
        ctx.exec
            .copy_to(
                &member.vmm_host,
                &member.vmm_host.host,
                &[ctx.local_op_bundle, local_cfg_path.as_path()],
                &remote_dir,
                false,
            )
            .wrap_err_with(|| {
                format!(
                    "upload operator bundle and full-node config to {}",
                    member.vmm_host.host
                )
            })?;
        let prep_cmd = ctx.target_bin.cmd(&format!(
            "disk prepare --role full-node --bundle {} --config {} --staging-name {} --size-gib {} --json",
            shell_quote(&remote_bundle),
            shell_quote(&remote_cfg),
            shell_quote(&member.vm_name),
            member.disk_gib,
        ));
        run_host_json(ctx.exec, &member.vmm_host, &member.name, &prep_cmd).wrap_err_with(|| {
            format!(
                "prepare keyless full-node data disk for `{}` on {}",
                member.vm_name, member.vmm_host.host
            )
        })
    })();
    let _ = cleanup_remote_dir(ctx.exec, &member.vmm_host, &remote_dir);
    prep_res
}

pub(super) fn start_provisioned_vm(
    exec: &dyn HostExec,
    bin: &RemoteOutbeUp,
    member: &DesiredMember,
    vm_id: &str,
    progress: &mut Progress,
) -> Result<()> {
    progress.stage(&format!(
        "starting VM `{}` ({vm_id}) on {}",
        member.vm_name, member.vmm_host.host
    ));
    let start_cmd = bin.cmd(&format!(
        "vm start --name {} --expect-vm-id {} --vmm-home {} --json",
        shell_quote(&member.vm_name),
        shell_quote(vm_id),
        shell_quote_tilde_path(&member.vmm_home),
    ));
    let _: RemoteVmStartOutput = run_host_json(exec, &member.vmm_host, &member.name, &start_cmd)
        .wrap_err_with(|| {
            format!(
                "start VM `{}` ({vm_id}) on {}",
                member.vm_name, member.vmm_host.host
            )
        })?;
    Ok(())
}

fn build_and_validate_new_member(
    build: &NetworkArtifacts,
    state: &NetworkVmState,
    existing_members: &[DesiredMember],
    spec: NewMemberSpec<'_>,
) -> Result<DesiredMember> {
    let vm_name = validate_new_member_basics(build, &spec)?;
    validate_new_member_ports(&spec)?;
    validate_new_member_collisions(existing_members, &vm_name, &spec)?;

    let prev = state.member(&vm_name);
    let next_index = prev.map(|m| m.index).unwrap_or_else(|| {
        existing_members
            .iter()
            .map(|m| m.index)
            .max()
            .map(|m| m + 1)
            .unwrap_or(build.config.founders.len())
    });

    Ok(DesiredMember {
        index: next_index,
        role: spec.role,
        name: spec.name.to_owned(),
        vm_name,
        owner_tag: member_owner_tag(
            &build.config.name,
            &build.manifest.genesis.final_sha256,
            spec.name,
        ),
        vmm_host: spec.vmm_host,
        vmm_home: spec.vmm_home,
        bridge: spec.bridge,
        service_ip: spec.service_ip,
        advertise_ip: spec.advertise_ip,
        rpc_port: spec.rpc_port,
        port_offset: spec.port_offset,
        vcpu: spec.vcpu,
        memory_gib: spec.memory_gib,
        disk_gib: spec.disk_gib,
        flavor: spec.flavor,
        no_tee: spec.no_tee,
        image_name: None,
        image_hash: None,
        recorded_disk_sha256: prev.and_then(|m| m.disk_source_sha256.clone()),
    })
}

fn validate_new_member_basics(
    build: &NetworkArtifacts,
    spec: &NewMemberSpec<'_>,
) -> Result<String> {
    ensure!(
        !spec.name.is_empty()
            && spec
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
        "invalid member name `{}` (expected letters, digits, `-`, or `_`)",
        spec.name
    );
    ensure!(
        !build.config.founders.iter().any(|f| f.name == spec.name),
        "member name `{}` collides with a founding validator",
        spec.name
    );
    let vm_name = founder_vm_name(&build.config.name, spec.name);
    ensure!(
        !vm_name.is_empty() && vm_name.len() <= 63,
        "computed VM name `{vm_name}` exceeds 63 characters"
    );
    ensure!(
        vm_name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric()),
        "computed VM name `{vm_name}` must start with an ASCII letter or digit"
    );
    ensure!(spec.vcpu > 0, "vcpu must be positive");
    ensure!(spec.memory_gib > 0, "memory_gib must be positive");
    ensure!(spec.disk_gib > 0, "disk_gib must be positive");
    ensure!(
        spec.service_ip.is_private() && !spec.service_ip.is_loopback(),
        "vm_ip ({}) must be a non-loopback private IPv4 address",
        spec.service_ip
    );

    let vm_defaults = build
        .config
        .vm
        .as_ref()
        .ok_or_else(|| eyre::eyre!("network config has no vm section"))?;
    let prefix = vm_defaults
        .prefix
        .ok_or_else(|| eyre::eyre!("missing vm.prefix in network config"))?;
    let gateway = vm_defaults
        .gateway
        .ok_or_else(|| eyre::eyre!("missing vm.gateway in network config"))?;
    StaticVmNetworkConfig {
        address: spec.service_ip,
        prefix_len: prefix,
        gateway,
        dns: vm_defaults.dns.clone(),
    }
    .validate()
    .wrap_err_with(|| format!("invalid static VM addressing for `{}`", spec.name))?;
    Ok(vm_name)
}

fn validate_new_member_ports(spec: &NewMemberSpec<'_>) -> Result<()> {
    let member_ports = if spec.role == MemberRole::FullNode {
        VmPortsConfig {
            rpc_port: spec.rpc_port,
            ..VmPortsConfig::default()
        }
    } else {
        let published = FounderPublishedPorts::from_offset(spec.port_offset)?;
        VmPortsConfig {
            rpc_port: spec.rpc_port,
            consensus_port: published.consensus_p2p_tcp,
            reth_port: published.reth_p2p_tcp,
            discv5_port: published.reth_discv5_udp,
            radicle_port: published.radicle_tcp,
        }
    };
    member_ports
        .validate()
        .wrap_err_with(|| format!("invalid rpc_port for `{}`", spec.name))
}

fn validate_new_member_collisions(
    existing_members: &[DesiredMember],
    vm_name: &str,
    spec: &NewMemberSpec<'_>,
) -> Result<()> {
    for other in existing_members {
        if other.vm_name == vm_name {
            ensure!(
                other.role == spec.role,
                "member `{}` already exists with role {:?}, cannot reuse as {:?}",
                spec.name,
                other.role,
                spec.role
            );
            continue;
        }
        ensure!(
            !(other.vmm_host.host == spec.vmm_host.host && other.service_ip == spec.service_ip),
            "vm_ip {} on host {} collides with existing member `{}`",
            spec.service_ip,
            spec.vmm_host.host,
            other.name
        );
        if spec.role == MemberRole::JoiningValidator && other.role != MemberRole::FullNode {
            check_validator_port_collision(spec, other)?;
        }
    }
    Ok(())
}

fn check_validator_port_collision(spec: &NewMemberSpec<'_>, other: &DesiredMember) -> Result<()> {
    let new_ports = FounderPublishedPorts::from_offset(spec.port_offset)?;
    let other_ports = FounderPublishedPorts::from_offset(other.port_offset)?;
    for (svc, proto, port) in new_ports.bindings() {
        for (osvc, oproto, oport) in other_ports.bindings() {
            if proto != oproto || port != oport {
                continue;
            }
            ensure!(
                other.vmm_host.host != spec.vmm_host.host,
                "port collision on host {}: `{}` ({svc}/{proto}:{port}) collides with `{}` ({osvc}/{oproto}:{oport}); pass a distinct --port-offset",
                spec.vmm_host.host,
                spec.name,
                other.name
            );
            ensure!(
                other.advertise_ip != spec.advertise_ip,
                "advertised port collision on {}: `{}` ({svc}/{proto}:{port}) collides with `{}` ({osvc}/{oproto}:{oport})",
                spec.advertise_ip,
                spec.name,
                other.name
            );
        }
    }
    Ok(())
}

fn resolve_target_ssh(
    host: &str,
    user: Option<&str>,
    port: u16,
    default_ssh: Option<&Ssh>,
) -> Result<Ssh> {
    let ssh = Ssh {
        host: host.to_owned(),
        user: user
            .map(ToOwned::to_owned)
            .or_else(|| default_ssh.map(|s| s.user.clone()))
            .unwrap_or_else(|| "ubuntu".to_owned()),
        port,
    };
    validate_ssh(&ssh, host)?;
    Ok(ssh)
}

fn verify_host_ready_and_clean_stale(
    exec: &dyn HostExec,
    ssh: &Ssh,
    bin: &RemoteOutbeUp,
    bridge: &str,
    vmm_home: &str,
) -> Result<()> {
    let status_cmd = bin.cmd(&format!(
        "host status --bridge {} --vmm-home {} --json",
        shell_quote(bridge),
        shell_quote_tilde_path(vmm_home),
    ));
    let host_status: RemoteHostStatusOutput = run_host_json(exec, ssh, &ssh.host, &status_cmd)
        .wrap_err_with(|| {
            format!(
                "inspect host readiness via `outbe-up host status` on {}",
                ssh.host
            )
        })?;
    ensure!(
        host_status.ready_for_vm,
        "VMM host {} is not ready for VMs (`outbe-up host status` reported ready_for_vm=false): {}",
        ssh.host,
        if host_status.notes.is_empty() {
            format!(
                "run `outbe-up host status --bridge {bridge}` on {} for details",
                ssh.host
            )
        } else {
            host_status.notes.join("; ")
        }
    );
    let stale_cmd = bin.cmd("disk clean --stale --json");
    let _: RemoteCleanStaleOutput = run_host_json(exec, ssh, &ssh.host, &stale_cmd)
        .wrap_err_with(|| format!("clean stale staging dirs on {}", ssh.host))?;
    Ok(())
}

fn resolve_image_on_target_host(
    exec: &dyn HostExec,
    target: &HostImageTarget<'_>,
    image_source: Option<&ImageSource>,
    state: &NetworkVmState,
) -> Result<RemoteImagePublishOutput> {
    if let Some(src) = image_source {
        let (resolved, _) = resolve_image_source(src)?;
        return publish_image_on_host(exec, target, &resolved);
    }

    ensure!(
        !state.image_hash.is_empty(),
        "network-vm-state.json has no recorded guest OS image hash; pass `--image-archive` or `--image-url --image-sha256`"
    );
    let list_cmd = target.bin.cmd(&format!(
        "image list --vmm-home {} --json",
        shell_quote_tilde_path(target.vmm_home),
    ));
    let listed: Vec<RemoteListedImage> =
        run_host_json(exec, target.ssh, &target.ssh.host, &list_cmd)
            .wrap_err_with(|| format!("list guest OS images on {}", target.ssh.host))?;
    let found = listed
        .into_iter()
        .find(|img| img.has_archive_manifest && img.os_image_hash == state.image_hash)
        .ok_or_else(|| {
            eyre::eyre!(
                "guest OS image `{}` ({}) is not present on {}; pass `--image-archive` or `--image-url --image-sha256`",
                state.image_name,
                state.image_hash,
                target.ssh.host
            )
        })?;
    Ok(RemoteImagePublishOutput {
        image_name: found.name,
        os_image_hash: found.os_image_hash,
        target_dir: found.path,
        reused: true,
    })
}

fn create_member_vm_from_staging(
    exec: &dyn HostExec,
    bin: &RemoteOutbeUp,
    member: &DesiredMember,
    host_img: &RemoteImagePublishOutput,
) -> Result<RemoteVmCreateOutput> {
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
        run_host_json(exec, &member.vmm_host, &member.name, &create_cmd).wrap_err_with(|| {
            format!("create VM `{}` on {}", member.vm_name, member.vmm_host.host)
        })?;
    ensure!(
        created.name == member.vm_name
            && created.image_name == host_img.image_name
            && created.os_image_hash == host_img.os_image_hash,
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
            exec,
            &member.vmm_host,
            &member.name,
            &clean_cmd,
        );
    }
    Ok(created)
}
