//! Remote single-VM graceful update execution, state persistence, and post-update lockstep wait.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::progress::Progress;
use outbe_operator_remote::{run_host_json, shell_quote, HostExec};

use super::super::{
    build_member_state, compute_readiness_baseline, host_key,
    members::{fetch_member_serial_tail, verify_finalized_hash_match},
    render_vm_status, shell_quote_tilde_path, DesiredMember, RemoteImagePublishOutput,
    RemoteOutbeUp, RemoteVmStatusOutput, RemoteVmUpdateOutput, READY_CONSECUTIVE_SAMPLES,
};
use super::{health::poll_network_until, NetworkSample, PollContext, PollStep, TargetImage};
use crate::network::vm_state::{MemberRole, NetworkVmState, NetworkVmStateLock};

pub(super) struct RemoteUpdateTarget<'a> {
    pub(super) member: &'a DesiredMember,
    pub(super) observed: &'a RemoteVmStatusOutput,
    pub(super) bin: &'a RemoteOutbeUp,
    pub(super) host_img: &'a RemoteImagePublishOutput,
}

pub(super) struct LockstepTarget<'a> {
    pub(super) updated_idx: usize,
    pub(super) updated_vm_id: &'a str,
    pub(super) min_finalized_blocks: u64,
}

pub(super) fn run_remote_vm_update(
    exec: &dyn HostExec,
    target: &RemoteUpdateTarget<'_>,
    image: &TargetImage,
    stop_timeout: Duration,
) -> Result<RemoteVmUpdateOutput> {
    let member = target.member;
    let observed = target.observed;
    let update_cmd = target.bin.cmd(&format!(
        "vm update --name {} --expect-vm-id {} --image {} --image-name {} --stop-timeout {} --vmm-home {} --json",
        shell_quote(&member.vm_name),
        shell_quote(&observed.vm_id),
        shell_quote(&target.host_img.target_dir),
        shell_quote(&image.name),
        stop_timeout.as_secs(),
        shell_quote_tilde_path(&member.vmm_home),
    ));
    let updated: RemoteVmUpdateOutput =
        run_host_json(exec, &member.vmm_host, &member.name, &update_cmd).wrap_err_with(|| {
            format!(
                "gracefully update VM `{}` ({}) on {} to `{}`",
                member.vm_name, observed.vm_id, member.vmm_host.host, image.name
            )
        })?;
    ensure!(
        updated.vm_id == observed.vm_id
            && updated.image_name == image.name
            && updated.os_image_hash == image.hash,
        "unexpected `outbe-up vm update` output for `{}` on {}: got vm_id={} image={} ({}), expected vm_id={} image={} ({})",
        member.vm_name,
        member.vmm_host.host,
        updated.vm_id,
        updated.image_name,
        updated.os_image_hash,
        observed.vm_id,
        image.name,
        image.hash
    );
    Ok(updated)
}

pub(super) fn record_member_target_image(
    member: &mut DesiredMember,
    (state, state_lock, artifacts): (&mut NetworkVmState, &NetworkVmStateLock, &Path),
    observed: &RemoteVmStatusOutput,
    image: &TargetImage,
) -> Result<()> {
    member.image_name = Some(image.name.clone());
    member.image_hash = Some(image.hash.clone());
    state.upsert_member(build_member_state(
        member,
        &image.hash,
        observed
            .disk_source_sha256
            .clone()
            .or_else(|| member.recorded_disk_sha256.clone()),
        Some(observed.vm_id.clone()),
        Some(observed.mac.clone()),
    ));
    state.save_locked(state_lock, artifacts)?;
    Ok(())
}

pub(super) async fn wait_post_vm_update_lockstep(
    ctx: &PollContext<'_>,
    target: &LockstepTarget<'_>,
    deadline: Instant,
    progress: &mut Progress,
) -> Result<()> {
    let updated_member = &ctx.desired_members[target.updated_idx];
    let ref_idx = ctx
        .desired_members
        .iter()
        .enumerate()
        .find(|(i, m)| *i != target.updated_idx && m.role != MemberRole::FullNode)
        .map(|(i, _)| i)
        .unwrap_or(target.updated_idx);

    let mut baseline: Option<u64> = None;
    let mut consecutive_ready: u32 = 0;

    let outcome = poll_network_until(ctx, deadline, progress, |sample| {
        if sample.fully_healthy() && baseline.is_none() {
            baseline = compute_readiness_baseline(&sample.statuses);
        }
        let sample_ok =
            post_update_lockstep_sample_ok(ctx, sample, target, ref_idx, baseline)?;
        consecutive_ready = if sample_ok { consecutive_ready + 1 } else { 0 };
        if consecutive_ready >= READY_CONSECUTIVE_SAMPLES {
            return Ok(PollStep::Done(()));
        }
        Ok(PollStep::Continue(format!(
            "post-update `{}`: min_finalized={:?} baseline={:?} target=+{} ready_streak={consecutive_ready}/{READY_CONSECUTIVE_SAMPLES}",
            updated_member.name,
            sample.min_finalized(),
            baseline,
            target.min_finalized_blocks
        )))
    })
    .await?;

    match outcome {
        Ok(()) => Ok(()),
        Err(sample) => {
            let hkey = host_key(&updated_member.vmm_host, &updated_member.vmm_home);
            let tail = fetch_member_serial_tail(
                ctx.exec.as_ref(),
                updated_member,
                target.updated_vm_id,
                &ctx.host_bins[&hkey],
            );
            bail!(
                "timed out waiting for `{}` recovery and lockstep after image update (stopping rollout before remaining VMs; baseline={:?}):\n\n{}{}",
                updated_member.name,
                baseline,
                render_vm_status(ctx.build, &sample.statuses, &sample.orphans, ctx.max_lag),
                tail,
            );
        }
    }
}

fn post_update_lockstep_sample_ok(
    ctx: &PollContext<'_>,
    sample: &NetworkSample,
    target: &LockstepTarget<'_>,
    ref_idx: usize,
    baseline: Option<u64>,
) -> Result<bool> {
    if !sample.fully_healthy() {
        return Ok(false);
    }
    let Some(b) = baseline else {
        return Ok(false);
    };
    let required_height = b.saturating_add(target.min_finalized_blocks);
    let growth_ok = sample
        .statuses
        .iter()
        .all(|s| s.finalized.is_some_and(|h| h >= required_height));
    if !growth_ok {
        return Ok(false);
    }
    if ref_idx == target.updated_idx {
        return Ok(true);
    }
    verify_finalized_hash_match(
        ctx.exec.as_ref(),
        &ctx.desired_members[target.updated_idx],
        &sample.statuses[target.updated_idx],
        &ctx.desired_members[ref_idx],
        &sample.statuses[ref_idx],
    )
}
