//! Multi-host VM founding-network start orchestration (`outbe-net-up network start`).

use std::{
    collections::BTreeMap,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::progress::Progress;
use outbe_operator_remote::{run_host_json, shell_quote, HostExec};
use tokio::task::JoinSet;

use super::{
    ensure_vm_deployment,
    host_check::{format_failed_host_checks, run_host_checks, PublishContext},
    host_key,
    readiness::{
        assess_members_lag, compute_readiness_baseline, evaluate_network_readiness,
        render_vm_status, ReadinessRequirements,
    },
    remote_bin::{
        ensure_pinned_outbe_up_on_host, fetch_host_vms, resolve_state_or_bundled_outbe_up_sha256,
        RemoteOutbeUp,
    },
    rpc::{ensure_vm_rpc_reachable_for_orchestration, verify_certified_rpc_chain_id},
    shell_quote_tilde_path,
    status::{query_members_and_orphans, VmMemberStatus},
    types::{
        build_all_desired_members, classify_host_vm, collect_unique_hosts, DesiredMember,
        RemoteVmLogsOutput, RemoteVmStartOutput, RemoteVmStatusOutput, UniqueHost,
        VmClassification,
    },
    StartOptions, READY_CONSECUTIVE_SAMPLES, SERIAL_LOG_TAIL_LINES,
};
use crate::network::{
    remote::NetworkArtifacts,
    vm_state::{MemberRole, NetworkVmState},
};

struct TimeoutReportContext<'a> {
    build: &'a NetworkArtifacts,
    targets: &'a [(DesiredMember, RemoteVmStatusOutput)],
    host_bins: &'a BTreeMap<String, RemoteOutbeUp>,
    exec: &'a dyn HostExec,
    timeout: Duration,
    statuses: &'a [VmMemberStatus],
    max_lag: u64,
    baseline_b: Option<u64>,
    min_finalized_blocks: u64,
}

pub(crate) async fn start_with_exec(
    options: &StartOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("loading network artifacts and reconciling hosts");
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;
    ensure_vm_rpc_reachable_for_orchestration(&build)?;

    let state_lock = NetworkVmState::lock(options.artifacts)?;
    let mut state = NetworkVmState::load_optional_locked(&state_lock, options.artifacts)?;
    if let Some(st) = &state {
        st.ensure_matches_network(&build)?;
    }
    let outbe_up_sha256 = resolve_state_or_bundled_outbe_up_sha256(&build.root, state.as_ref())?;

    let desired_members = build_all_desired_members(&build, state.as_ref(), None, None, None)?;
    let unique_hosts = collect_unique_hosts(&desired_members);
    let host_bins =
        pin_outbe_up_on_hosts(exec.as_ref(), &build.root, &outbe_up_sha256, &unique_hosts)?;

    let live_targets = reconcile_start_targets(
        exec.as_ref(),
        options.artifacts,
        &desired_members,
        (&unique_hosts, &host_bins),
        state.as_mut(),
    )?;
    if let Some(st) = &state {
        let _ = st.save_locked(&state_lock, options.artifacts);
    }
    drop(state_lock);

    verify_start_host_checks(&mut progress, &desired_members, &host_bins, exec.as_ref())?;
    start_vms_in_parallel(&mut progress, &live_targets, &host_bins, &exec).await?;
    let poll_env = ReadinessPollEnv {
        options,
        build: &build,
        desired_members: &desired_members,
        unique_hosts: &unique_hosts,
        host_bins: &host_bins,
        live_targets: &live_targets,
        exec: &exec,
    };
    poll_readiness_loop(&mut progress, &poll_env).await
}

fn pin_outbe_up_on_hosts(
    exec: &dyn HostExec,
    artifacts_root: &Path,
    outbe_up_sha256: &str,
    unique_hosts: &BTreeMap<String, UniqueHost>,
) -> Result<BTreeMap<String, RemoteOutbeUp>> {
    let mut host_bins = BTreeMap::new();
    for (hkey, host) in unique_hosts {
        let bin =
            ensure_pinned_outbe_up_on_host(exec, &host.ssh, artifacts_root, outbe_up_sha256, true)?;
        host_bins.insert(hkey.clone(), bin);
    }
    Ok(host_bins)
}

fn reconcile_start_targets(
    exec: &dyn HostExec,
    artifacts: &Path,
    desired_members: &[DesiredMember],
    (unique_hosts, host_bins): (
        &BTreeMap<String, UniqueHost>,
        &BTreeMap<String, RemoteOutbeUp>,
    ),
    mut state: Option<&mut NetworkVmState>,
) -> Result<Vec<(DesiredMember, RemoteVmStatusOutput)>> {
    let mut host_vms_cache = BTreeMap::new();
    for (hkey, host) in unique_hosts {
        let vms = fetch_host_vms(exec, &host.ssh, &host_bins[hkey], &host.vmm_home)?;
        host_vms_cache.insert(hkey.clone(), vms);
    }

    let mut live_targets = Vec::with_capacity(desired_members.len());
    let mut problems = Vec::new();
    for member in desired_members {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let observed = host_vms_cache[&hkey]
            .iter()
            .find(|vm| vm.name == member.vm_name);
        match classify_host_vm(member, observed) {
            VmClassification::Ours { observed } => {
                if let Some(st) = state.as_deref_mut() {
                    if let Some(m) = st.members.iter_mut().find(|m| m.vm_name == member.vm_name) {
                        m.vm_id = Some(observed.vm_id.clone());
                        m.mac = Some(observed.mac.clone());
                    }
                }
                live_targets.push((member.clone(), *observed));
            }
            other => problems.push(format_start_reconcile_problem(member, &other, artifacts)),
        }
    }
    ensure!(
        problems.is_empty(),
        "cannot start VM network: host reconciliation found non-ready members:\n  - {}",
        problems.join("\n  - ")
    );
    Ok(live_targets)
}

fn format_start_reconcile_problem(
    member: &DesiredMember,
    class: &VmClassification,
    artifacts: &Path,
) -> String {
    match class {
        VmClassification::Absent => format!(
            "{} (`{}` on {}): absent (run `outbe-net-up network deploy --artifacts {}`)",
            member.name,
            member.vm_name,
            member.vmm_host.host,
            artifacts.display()
        ),
        VmClassification::Partial { observed } => format!(
            "{} (`{}` on {}): partial phase `{}` (resume `network deploy`)",
            member.name, member.vm_name, member.vmm_host.host, observed.phase
        ),
        VmClassification::Foreign { reason, .. } => {
            format!("{} (`{}`): foreign ({reason})", member.name, member.vm_name)
        }
        VmClassification::Stale { reasons, .. } => format!(
            "{} (`{}`): stale ({})",
            member.name,
            member.vm_name,
            reasons.join(", ")
        ),
        VmClassification::Ours { .. } => unreachable!("handled above"),
    }
}

fn verify_start_host_checks(
    progress: &mut Progress,
    desired_members: &[DesiredMember],
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    exec: &dyn HostExec,
) -> Result<()> {
    progress.stage("verifying host networking and firewall rules (`outbe-up host check`)");
    let publish_ctx = PublishContext::from_members(desired_members);
    let checks = run_host_checks(desired_members, host_bins, exec, &publish_ctx)?;
    let failed_count = checks.iter().filter(|(_, c)| !c.ready).count();
    if failed_count > 0 {
        let details = format_failed_host_checks(&checks);
        bail!(
            "refusing to start VM network: `outbe-up host check` failed for {failed_count}/{} VM(s). Apply the recommended host configuration and re-run `outbe-net-up network start`:\n{details}",
            checks.len()
        );
    }
    Ok(())
}

async fn start_vms_in_parallel(
    progress: &mut Progress,
    live_targets: &[(DesiredMember, RemoteVmStatusOutput)],
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    exec: &Arc<dyn HostExec>,
) -> Result<()> {
    progress.stage(&format!(
        "starting {} founding validator VM(s) in parallel",
        live_targets.len()
    ));
    let mut start_tasks = JoinSet::new();
    for (member, vm) in live_targets {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let bin = host_bins[&hkey].clone();
        let exec = Arc::clone(exec);
        let member = member.clone();
        let vm_id = vm.vm_id.clone();
        start_tasks.spawn_blocking(move || {
            let cmd = bin.cmd(&format!(
                "vm start --name {} --expect-vm-id {} --vmm-home {} --json",
                shell_quote(&member.vm_name),
                shell_quote(&vm_id),
                shell_quote_tilde_path(&member.vmm_home),
            ));
            let _: RemoteVmStartOutput =
                run_host_json(exec.as_ref(), &member.vmm_host, &member.name, &cmd).wrap_err_with(
                    || {
                        format!(
                            "start VM `{}` ({vm_id}) on {}",
                            member.vm_name, member.vmm_host.host
                        )
                    },
                )?;
            Ok::<(), eyre::Report>(())
        });
    }
    while let Some(res) = start_tasks.join_next().await {
        res.wrap_err("VM start task panicked")??;
    }
    Ok(())
}

struct ReadinessPollEnv<'a> {
    options: &'a StartOptions<'a>,
    build: &'a NetworkArtifacts,
    desired_members: &'a [DesiredMember],
    unique_hosts: &'a BTreeMap<String, UniqueHost>,
    host_bins: &'a BTreeMap<String, RemoteOutbeUp>,
    live_targets: &'a [(DesiredMember, RemoteVmStatusOutput)],
    exec: &'a Arc<dyn HostExec>,
}

async fn poll_readiness_loop(progress: &mut Progress, env: &ReadinessPollEnv<'_>) -> Result<()> {
    progress.stage(&format!(
        "waiting for finalized consensus growth (+{} blocks across {} consecutive samples)",
        env.options.min_finalized_blocks, READY_CONSECUTIVE_SAMPLES
    ));
    let deadline = Instant::now() + env.options.timeout;
    let required_peers = env
        .live_targets
        .iter()
        .filter(|(m, _)| m.role != MemberRole::FullNode)
        .count()
        .saturating_sub(1);
    let mut baseline_b: Option<u64> = None;
    let mut consecutive_ready: u32 = 0;

    loop {
        let (statuses, _) = query_members_and_orphans(
            env.build,
            env.desired_members,
            env.unique_hosts,
            env.host_bins,
            env.exec,
        )
        .await?;
        let assessments = assess_members_lag(&statuses, env.options.max_lag);
        if baseline_b.is_none() {
            baseline_b = compute_readiness_baseline(&statuses);
        }

        let sample_ready = evaluate_network_readiness(
            &statuses,
            &assessments,
            ReadinessRequirements {
                chain_id: env.build.config.chain_id,
                required_peers,
                baseline_b,
                min_finalized_blocks: env.options.min_finalized_blocks,
            },
        );
        consecutive_ready = if sample_ready {
            consecutive_ready + 1
        } else {
            0
        };
        if consecutive_ready >= READY_CONSECUTIVE_SAMPLES {
            return finish_start_ready(progress, env, &statuses, baseline_b, consecutive_ready)
                .await;
        }

        if Instant::now() >= deadline {
            bail!(
                "{}",
                build_timeout_report(&TimeoutReportContext {
                    build: env.build,
                    targets: env.live_targets,
                    host_bins: env.host_bins,
                    exec: env.exec.as_ref(),
                    timeout: env.options.timeout,
                    statuses: &statuses,
                    max_lag: env.options.max_lag,
                    baseline_b,
                    min_finalized_blocks: env.options.min_finalized_blocks,
                })
            );
        }

        let min_fin = statuses
            .iter()
            .filter_map(|s| s.finalized)
            .min()
            .map_or_else(|| "-".to_owned(), |h| h.to_string());
        let base_str = baseline_b.map_or_else(|| "-".to_owned(), |b| b.to_string());
        progress.update(&format!(
            "baseline={base_str} min_finalized={min_fin} target_growth=+{} ready_streak={consecutive_ready}/{READY_CONSECUTIVE_SAMPLES}",
            env.options.min_finalized_blocks,
        ));

        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            tokio::time::sleep(remaining.min(Duration::from_secs(5))).await;
        }
    }
}

async fn finish_start_ready(
    progress: &mut Progress,
    env: &ReadinessPollEnv<'_>,
    statuses: &[VmMemberStatus],
    baseline_b: Option<u64>,
    consecutive_ready: u32,
) -> Result<()> {
    progress.stage(&format!(
        "verifying certified_rpc ({}) answers eth_chainId == {}",
        env.build.config.certified_rpc, env.build.config.chain_id
    ));
    verify_certified_rpc_chain_id(
        env.build,
        &env.desired_members[0].vmm_host,
        env.exec.as_ref(),
    )
    .await?;
    progress.done(&format!(
        "VM network {} finalized (baseline B={}, growth >= +{}, {} consecutive ready samples)",
        env.build.config.name,
        baseline_b.unwrap_or(0),
        env.options.min_finalized_blocks,
        consecutive_ready
    ));
    println!(
        "\n{}",
        render_vm_status(env.build, statuses, &[], env.options.max_lag)
    );
    Ok(())
}

fn build_timeout_report(ctx: &TimeoutReportContext<'_>) -> String {
    let mut out = format!(
        "timed out after {}s waiting for finalized consensus growth (baseline={:?}, required=+{}):\n\n{}",
        ctx.timeout.as_secs(),
        ctx.baseline_b,
        ctx.min_finalized_blocks,
        render_vm_status(ctx.build, ctx.statuses, &[], ctx.max_lag)
    );
    for (member, vm) in ctx.targets {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let bin = &ctx.host_bins[&hkey];
        let cmd = bin.cmd(&format!(
            "vm logs --name {} --expect-vm-id {} --lines {} --vmm-home {} --json",
            shell_quote(&member.vm_name),
            shell_quote(&vm.vm_id),
            SERIAL_LOG_TAIL_LINES,
            shell_quote_tilde_path(&member.vmm_home),
        ));
        let Ok(logs) = run_host_json::<dyn HostExec, RemoteVmLogsOutput>(
            ctx.exec,
            &member.vmm_host,
            &member.name,
            &cmd,
        ) else {
            continue;
        };
        if !logs.log.trim().is_empty() {
            out.push_str(&format!(
                "\n\n--- serial log tail for {} ({}) ---\n{}",
                member.vm_name,
                member.vmm_host.host,
                logs.log.trim_end()
            ));
        }
    }
    out
}
