//! Keyless full-node VM orchestration (`outbe-net-up network add-full-node`) and shared hash/log helpers.

use std::{sync::Arc, time::Instant};

use eyre::{bail, Result};
use outbe_operator_node_role::progress::Progress;
use outbe_operator_remote::{run_host_json, shell_quote, HostExec};

use super::super::{
    collect_unique_hosts, compute_readiness_baseline, member_node_ready, query_members_and_orphans,
    query_vm_block_hash, render_vm_status, shell_quote_tilde_path, DesiredMember, RemoteOutbeUp,
    RemoteVmLogsOutput, VmClassKind, VmMemberStatus, VmRpcTarget, READY_CONSECUTIVE_SAMPLES,
    SERIAL_LOG_TAIL_LINES,
};
use super::{
    load_and_verify_member_preflight,
    provision::{
        provision_member, start_provisioned_vm, MemberDiskSource, ProvisionRequest,
        ProvisionedMember,
    },
    resolve_member_host_bins, sleep_until_poll_tick, AddFullNodeOptions,
};
use crate::network::vm_state::MemberRole;

/// Provision and start a keyless full-node VM (zero provisioned keys; Reth generates its own
/// ephemeral P2P key inside `--datadir`) and verify it tracks founder-0 finalized block hashes.
pub(crate) async fn add_full_node_with_exec(
    options: &AddFullNodeOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    let deadline = Instant::now() + options.timeout;
    let mut progress = Progress::new();
    let _ =
        load_and_verify_member_preflight(options.artifacts, exec.as_ref(), &mut progress).await?;
    let advertise_ip = options.advertise_ip.unwrap_or(options.vm_ip);

    let ProvisionedMember {
        build,
        member,
        all_members,
        target_hkey,
        target_bin,
        outbe_up_sha256,
        vm_id,
    } = provision_member(
        &ProvisionRequest {
            artifacts: options.artifacts,
            role: MemberRole::FullNode,
            name: options.name,
            host: options.host,
            user: options.user,
            port: options.port,
            vmm_home: options.vmm_home,
            bridge: options.bridge,
            vm_ip: options.vm_ip,
            rpc_bind_ip: options.rpc_bind_ip,
            advertise_ip,
            rpc_port: options.rpc_port,
            port_offset: 0,
            image_source: options.image_source.as_ref(),
            vcpu: options.vcpu,
            memory_gib: options.memory_gib,
            disk_gib: options.disk_gib,
            no_tee: options.no_tee,
        },
        &MemberDiskSource::FullNodeKeyless,
        exec.as_ref(),
        &mut progress,
    )?;

    start_provisioned_vm(exec.as_ref(), &target_bin, &member, &vm_id, &mut progress)?;

    let founder0 = all_members
        .iter()
        .find(|m| m.role == MemberRole::Founder)
        .cloned()
        .ok_or_else(|| eyre::eyre!("no founding validator found"))?;
    let pair = vec![founder0.clone(), member.clone()];
    let unique_pair_hosts = collect_unique_hosts(&pair);
    let pair_bins = resolve_member_host_bins(
        exec.as_ref(),
        &build.root,
        &outbe_up_sha256,
        &unique_pair_hosts,
        (target_hkey, target_bin.clone()),
    )?;

    progress.stage(&format!(
        "verifying keyless full node `{}` tracks finalized height (+{} blocks, matching hashes)",
        member.name, options.min_finalized_blocks
    ));
    let mut baseline_b: Option<u64> = None;
    let mut consecutive_ready: u32 = 0;

    loop {
        let (statuses, _) =
            query_members_and_orphans(&build, &pair, &unique_pair_hosts, &pair_bins, &exec).await?;
        let f0_status = statuses.iter().find(|s| s.vm_name == founder0.vm_name);
        let fn_status = statuses.iter().find(|s| s.vm_name == member.vm_name);

        if baseline_b.is_none() {
            baseline_b = compute_readiness_baseline(&statuses);
        }

        let sample_ready = full_node_sample_ready(
            exec.as_ref(),
            (&member, &founder0),
            (fn_status, f0_status),
            (
                build.config.chain_id,
                baseline_b,
                options.min_finalized_blocks,
                options.max_lag,
            ),
        )?;

        consecutive_ready = if sample_ready {
            consecutive_ready + 1
        } else {
            0
        };
        if consecutive_ready >= READY_CONSECUTIVE_SAMPLES {
            progress.done(&format!(
                "keyless full node `{}` synced and tracking finalized blocks (baseline B={}, growth >= +{})",
                member.name,
                baseline_b.unwrap_or(0),
                options.min_finalized_blocks,
            ));
            println!(
                "\n{}",
                render_vm_status(&build, &statuses, &[], options.max_lag)
            );
            return Ok(());
        }

        if Instant::now() >= deadline {
            let tail = fetch_member_serial_tail(exec.as_ref(), &member, &vm_id, &target_bin);
            bail!(
                "timed out after {}s waiting for keyless full node `{}` to track finalized blocks (baseline={:?}, required=+{}):\n\n{}{}",
                options.timeout.as_secs(),
                member.name,
                baseline_b,
                options.min_finalized_blocks,
                render_vm_status(&build, &statuses, &[], options.max_lag),
                tail,
            );
        }

        progress.update(&format!(
            "full_node_finalized={:?} founder0_finalized={:?} baseline={:?} ready_streak={consecutive_ready}/{READY_CONSECUTIVE_SAMPLES}",
            fn_status.and_then(|s| s.finalized),
            f0_status.and_then(|s| s.finalized),
            baseline_b,
        ));
        sleep_until_poll_tick(deadline).await;
    }
}

fn full_node_sample_ready(
    exec: &dyn HostExec,
    (member, founder0): (&DesiredMember, &DesiredMember),
    (fn_status, f0_status): (Option<&VmMemberStatus>, Option<&VmMemberStatus>),
    (chain_id, baseline_b, min_finalized_blocks, max_lag): (u64, Option<u64>, u64, u64),
) -> Result<bool> {
    let (Some(b), Some(f0), Some(fns)) = (baseline_b, f0_status, fn_status) else {
        return Ok(false);
    };
    if fns.classification != VmClassKind::Ours || !member_node_ready(fns, chain_id, 0) {
        return Ok(false);
    }
    let (Some(f0_h), Some(fn_h)) = (f0.finalized, fns.finalized) else {
        return Ok(false);
    };
    let target = b.saturating_add(min_finalized_blocks);
    let lag = f0_h.saturating_sub(fn_h);
    if fn_h < target || lag > max_lag {
        return Ok(false);
    }
    verify_finalized_hash_match(exec, member, fns, founder0, f0)
}

/// Verify two VMs agree on the exact canonical block hash at `min(a.finalized, b.finalized)`.
pub(crate) fn verify_finalized_hash_match(
    exec: &dyn HostExec,
    member_a: &DesiredMember,
    status_a: &VmMemberStatus,
    member_b: &DesiredMember,
    status_b: &VmMemberStatus,
) -> Result<bool> {
    let (Some(ha), Some(hb)) = (status_a.finalized, status_b.finalized) else {
        return Ok(false);
    };
    if ha == hb {
        if let (Some(hash_a), Some(hash_b)) = (
            status_a.finalized_hash.as_deref(),
            status_b.finalized_hash.as_deref(),
        ) {
            return Ok(hash_a == hash_b);
        }
    }
    let check_height = ha.min(hb);
    let hash_a = query_vm_block_hash(
        exec,
        VmRpcTarget {
            ssh: &member_a.vmm_host,
            label: &member_a.name,
            service_ip: status_a.service_ip,
            rpc_port: member_a.rpc_port,
        },
        check_height,
    )?;
    let hash_b = query_vm_block_hash(
        exec,
        VmRpcTarget {
            ssh: &member_b.vmm_host,
            label: &member_b.name,
            service_ip: status_b.service_ip,
            rpc_port: member_b.rpc_port,
        },
        check_height,
    )?;
    Ok(hash_a == hash_b)
}

pub(crate) fn fetch_member_serial_tail(
    exec: &dyn HostExec,
    member: &DesiredMember,
    vm_id: &str,
    bin: &RemoteOutbeUp,
) -> String {
    let cmd = bin.cmd(&format!(
        "vm logs --name {} --expect-vm-id {} --lines {} --vmm-home {} --json",
        shell_quote(&member.vm_name),
        shell_quote(vm_id),
        SERIAL_LOG_TAIL_LINES,
        shell_quote_tilde_path(&member.vmm_home),
    ));
    let Ok(logs) = run_host_json::<dyn HostExec, RemoteVmLogsOutput>(
        exec,
        &member.vmm_host,
        &member.name,
        &cmd,
    ) else {
        return String::new();
    };
    if logs.log.trim().is_empty() {
        return String::new();
    }
    format!(
        "\n\n--- serial log tail for {} ({}) ---\n{}",
        member.vm_name,
        member.vmm_host.host,
        logs.log.trim_end()
    )
}
