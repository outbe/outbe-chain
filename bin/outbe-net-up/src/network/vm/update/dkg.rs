//! Pre-stop DKG boundary gate, forward missed-vote observation, and post-rollout DKG verification.

use std::{collections::BTreeMap, time::Instant};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::progress::Progress;

use super::super::{
    members::verify_finalized_hash_match, query_validators_missed_votes, render_vm_status,
    DesiredMember, VmMemberStatus,
};
use super::{
    health::{
        all_validators_randomness_healthy, check_validators_connected_peers,
        ensure_all_members_healthy, poll_network_until, sample_network,
    },
    NetworkSample, PollContext, PollStep,
};
use crate::network::vm_state::MemberRole;

/// Forward finalized-block observation window for verifying zero missed votes across all
/// validators (`IValidatorSet.validatorByAddress`) before stopping any VM.
const MISSED_VOTE_OBSERVATION_BLOCKS: u64 = 60;

enum MissedVoteOutcome {
    Clean,
    NeedsDkgWait(Option<String>),
}

struct ValidatorDkgSnapshot {
    finalized: u64,
    next_activation: u64,
    randomness_healthy: bool,
    in_ceremony: bool,
}

fn read_validator_dkg_snapshot(
    sample: &NetworkSample,
    member: &DesiredMember,
) -> Result<ValidatorDkgSnapshot> {
    let target_status = sample
        .statuses
        .iter()
        .find(|s| s.vm_name == member.vm_name)
        .ok_or_else(|| {
            eyre::eyre!(
                "refusing to stop validator `{}`: missing member status (fail-closed DKG guard)",
                member.name
            )
        })?;
    let consensus = target_status.consensus.as_ref().ok_or_else(|| {
        eyre::eyre!(
            "refusing to stop validator `{}`: missing `outbe_consensusStatus` (fail-closed DKG guard)",
            member.name
        )
    })?;
    let finalized = target_status.finalized.ok_or_else(|| {
        eyre::eyre!(
            "refusing to stop validator `{}`: missing finalized block height (fail-closed DKG guard)",
            member.name
        )
    })?;
    let next_activation = consensus.next_planned_activation_height;
    ensure!(
        next_activation > 0,
        "refusing to stop validator `{}`: `next_planned_activation_height` is 0 or missing in `outbe_consensusStatus` (fail-closed DKG guard)",
        member.name
    );
    let randomness_healthy = all_validators_randomness_healthy(&sample.statuses);
    let in_ceremony = !randomness_healthy
        || consensus
            .last_dkg_activation_height
            .is_some_and(|last| last >= next_activation);
    Ok(ValidatorDkgSnapshot {
        finalized,
        next_activation,
        randomness_healthy,
        in_ceremony,
    })
}

fn dkg_boundary_cleared(
    sample: &NetworkSample,
    snap: &ValidatorDkgSnapshot,
    boundary: u64,
    required_peers: usize,
) -> bool {
    let peers_ok = check_validators_connected_peers(&sample.statuses, required_peers).is_ok();
    let passed_boundary = snap.finalized > boundary && snap.next_activation > boundary;
    sample.fully_healthy() && peers_ok && snap.randomness_healthy && passed_boundary
}

/// Pre-stop gate run immediately before EVERY VM stop (F2 + F4 + forward missed-vote gate).
pub(super) async fn pre_stop_health_and_dkg_gate(
    ctx: &PollContext<'_>,
    member: &DesiredMember,
    effective_dkg_guard: u64,
    deadline: Instant,
    progress: &mut Progress,
) -> Result<()> {
    let mut last_offenders: Option<String> = None;

    loop {
        let initial_sample = acquire_pre_stop_sample(
            ctx,
            member,
            (effective_dkg_guard, deadline, last_offenders.as_deref()),
            progress,
        )
        .await?;

        if !initial_sample.has_validators {
            return Ok(());
        }

        let mut observer =
            MissedVoteObserver::start(ctx, member, &initial_sample, effective_dkg_guard, progress)?;
        match observer
            .run_until_clean(deadline, progress, &mut last_offenders)
            .await?
        {
            MissedVoteOutcome::Clean => return Ok(()),
            MissedVoteOutcome::NeedsDkgWait(note) => {
                if let Some(msg) = note {
                    progress.update(&msg);
                }
            }
        }
    }
}

async fn acquire_pre_stop_sample(
    ctx: &PollContext<'_>,
    member: &DesiredMember,
    (effective_dkg_guard, deadline, last_offenders): (u64, Instant, Option<&str>),
    progress: &mut Progress,
) -> Result<NetworkSample> {
    if member.role == MemberRole::FullNode {
        let sample = sample_network(ctx).await?;
        ensure_all_members_healthy(
            ctx.build,
            &sample,
            ctx.required_peers,
            ctx.max_lag,
            &format!("pre-stop health check before updating `{}`", member.name),
        )?;
        check_validators_connected_peers(&sample.statuses, ctx.required_peers)?;
        return Ok(sample);
    }

    let mut waited_boundary: Option<u64> = None;
    let outcome = poll_network_until(ctx, deadline, progress, |sample| {
        evaluate_dkg_gate_step(
            ctx,
            sample,
            member,
            effective_dkg_guard,
            &mut waited_boundary,
        )
    })
    .await?;

    match outcome {
        Ok(sample) => Ok(sample),
        Err(sample) => {
            let offenders_note = last_offenders
                .map(|o| format!("; last missed-vote deltas: {o}"))
                .unwrap_or_default();
            bail!(
                "timed out waiting to clear DKG activation boundary {:?} before updating `{}`{offenders_note}:\n\n{}",
                waited_boundary,
                member.name,
                render_vm_status(ctx.build, &sample.statuses, &sample.orphans, ctx.max_lag)
            );
        }
    }
}

fn evaluate_dkg_gate_step(
    ctx: &PollContext<'_>,
    sample: &NetworkSample,
    member: &DesiredMember,
    effective_dkg_guard: u64,
    waited_boundary: &mut Option<u64>,
) -> Result<PollStep<NetworkSample>> {
    if waited_boundary.is_none() {
        ensure_all_members_healthy(
            ctx.build,
            sample,
            ctx.required_peers,
            ctx.max_lag,
            &format!("pre-stop health check before updating `{}`", member.name),
        )?;
        check_validators_connected_peers(&sample.statuses, ctx.required_peers)?;
    }

    let snap = read_validator_dkg_snapshot(sample, member)?;
    if let Some(boundary) = *waited_boundary {
        if dkg_boundary_cleared(sample, &snap, boundary, ctx.required_peers) {
            return Ok(PollStep::Done(sample.clone_light()));
        }
        return Ok(PollStep::Continue(format!(
            "waiting past DKG boundary {boundary} before updating `{}` (finalized={}, next_activation={}, randomness_healthy={})",
            member.name, snap.finalized, snap.next_activation, snap.randomness_healthy
        )));
    }

    let remaining = snap.next_activation.saturating_sub(snap.finalized);
    let guard_with_obs = effective_dkg_guard.saturating_add(MISSED_VOTE_OBSERVATION_BLOCKS);
    if snap.in_ceremony || remaining <= guard_with_obs {
        *waited_boundary = Some(snap.next_activation);
        return Ok(PollStep::Continue(format!(
            "deferring stop of `{}` near/during DKG ceremony (finalized={}, next_activation={}, remaining={remaining}, effective_guard={effective_dkg_guard}, observation_window={MISSED_VOTE_OBSERVATION_BLOCKS}, randomness_healthy={})",
            member.name, snap.finalized, snap.next_activation, snap.randomness_healthy
        )));
    }

    Ok(PollStep::Done(sample.clone_light()))
}

struct MissedVoteObserver<'a> {
    ctx: &'a PollContext<'a>,
    member: &'a DesiredMember,
    probe: &'a DesiredMember,
    guard_with_obs: u64,
    start_height: u64,
    start_vrf: u64,
    start_missed: BTreeMap<String, u64>,
}

impl<'a> MissedVoteObserver<'a> {
    fn start(
        ctx: &'a PollContext<'a>,
        member: &'a DesiredMember,
        initial_sample: &NetworkSample,
        effective_dkg_guard: u64,
        progress: &mut Progress,
    ) -> Result<Self> {
        let probe = ctx
            .desired_members
            .iter()
            .find(|m| m.vm_name == member.vm_name && m.role != MemberRole::FullNode)
            .or_else(|| {
                ctx.desired_members
                    .iter()
                    .find(|m| m.role != MemberRole::FullNode)
            })
            .ok_or_else(|| {
                eyre::eyre!(
                    "refusing to stop `{}`: no validator available to query participation (fail-closed)",
                    member.name
                )
            })?;
        let start_height = initial_sample.min_finalized().ok_or_else(|| {
            eyre::eyre!(
                "refusing to stop `{}`: missing finalized height for missed-vote observation (fail-closed)",
                member.name
            )
        })?;
        let start_vrf = initial_sample.common_vrf.ok_or_else(|| {
            eyre::eyre!(
                "refusing to stop `{}`: missing common vrf_material_version for missed-vote observation (fail-closed)",
                member.name
            )
        })?;
        let start_missed = query_missed_votes_checked(ctx, probe, member, "initial")?;
        progress.update(&format!(
            "observing {} finalized blocks for 0 missed votes across {} validator(s) before updating `{}` (start={start_height}, vrf={start_vrf})",
            MISSED_VOTE_OBSERVATION_BLOCKS,
            start_missed.len(),
            member.name
        ));
        Ok(Self {
            ctx,
            member,
            probe,
            guard_with_obs: effective_dkg_guard.saturating_add(MISSED_VOTE_OBSERVATION_BLOCKS),
            start_height,
            start_vrf,
            start_missed,
        })
    }

    async fn run_until_clean(
        &mut self,
        deadline: Instant,
        progress: &mut Progress,
        last_offenders: &mut Option<String>,
    ) -> Result<MissedVoteOutcome> {
        let ctx = self.ctx;
        let outcome = poll_network_until(ctx, deadline, progress, |sample| {
            self.step(sample, last_offenders)
        })
        .await?;

        match outcome {
            Ok(res) => Ok(res),
            Err(sample) => {
                let offenders_detail = last_offenders
                    .as_deref()
                    .map(|o| format!("; last offending missed-vote deltas: {o}"))
                    .unwrap_or_default();
                bail!(
                    "refusing to stop `{}`: timed out observing {} clean finalized blocks with 0 missed votes (start_height={}, start_vrf={}{offenders_detail}):\n\n{}",
                    self.member.name,
                    MISSED_VOTE_OBSERVATION_BLOCKS,
                    self.start_height,
                    self.start_vrf,
                    render_vm_status(ctx.build, &sample.statuses, &sample.orphans, ctx.max_lag)
                );
            }
        }
    }

    fn needs_dkg_wait(
        &self,
        sample: &NetworkSample,
        cur_height: u64,
        check_margin: bool,
    ) -> Result<bool> {
        if self.member.role == MemberRole::FullNode {
            return Ok(false);
        }
        let snap = read_validator_dkg_snapshot(sample, self.member)?;
        let within_margin =
            check_margin && snap.next_activation.saturating_sub(cur_height) <= self.guard_with_obs;
        Ok(snap.in_ceremony || within_margin)
    }

    fn reset_window(&mut self, height: u64, vrf: u64, missed: BTreeMap<String, u64>) {
        self.start_height = height;
        self.start_vrf = vrf;
        self.start_missed = missed;
    }

    fn step(
        &mut self,
        sample: &NetworkSample,
        last_offenders: &mut Option<String>,
    ) -> Result<PollStep<MissedVoteOutcome>> {
        ensure_all_members_healthy(
            self.ctx.build,
            sample,
            self.ctx.required_peers,
            self.ctx.max_lag,
            &format!(
                "pre-stop missed-vote observation before updating `{}`",
                self.member.name
            ),
        )?;
        check_validators_connected_peers(&sample.statuses, self.ctx.required_peers)?;

        let cur_height = sample.min_finalized().ok_or_else(|| {
            eyre::eyre!(
                "refusing to stop `{}`: missing finalized height during missed-vote observation",
                self.member.name
            )
        })?;
        let cur_vrf = sample.common_vrf.ok_or_else(|| {
            eyre::eyre!(
                "refusing to stop `{}`: lost common vrf_material_version during missed-vote observation",
                self.member.name
            )
        })?;

        if cur_vrf != self.start_vrf {
            return self.handle_vrf_boundary_crossed(sample, cur_height, cur_vrf);
        }

        let target_height = self
            .start_height
            .saturating_add(MISSED_VOTE_OBSERVATION_BLOCKS);
        if cur_height < target_height {
            return Ok(PollStep::Continue(format!(
                "observing missed votes before updating `{}`: finalized={cur_height}/{target_height} (vrf={cur_vrf})",
                self.member.name
            )));
        }

        self.evaluate_completed_window(sample, cur_height, cur_vrf, last_offenders)
    }

    fn handle_vrf_boundary_crossed(
        &mut self,
        sample: &NetworkSample,
        cur_height: u64,
        cur_vrf: u64,
    ) -> Result<PollStep<MissedVoteOutcome>> {
        if self.needs_dkg_wait(sample, cur_height, true)? {
            return Ok(PollStep::Done(MissedVoteOutcome::NeedsDkgWait(None)));
        }
        let fresh = query_missed_votes_checked(self.ctx, self.probe, self.member, "post-boundary")?;
        self.reset_window(cur_height, cur_vrf, fresh);
        Ok(PollStep::Continue(format!(
            "epoch boundary crossed during missed-vote window before updating `{}`; retrying {} blocks from height {} (vrf={})",
            self.member.name, MISSED_VOTE_OBSERVATION_BLOCKS, self.start_height, self.start_vrf
        )))
    }

    fn evaluate_completed_window(
        &mut self,
        sample: &NetworkSample,
        cur_height: u64,
        cur_vrf: u64,
        last_offenders: &mut Option<String>,
    ) -> Result<PollStep<MissedVoteOutcome>> {
        let end_missed =
            query_missed_votes_checked(self.ctx, self.probe, self.member, "end-of-window")?;
        if participation_counters_reset(&self.start_missed, &end_missed) {
            if self.needs_dkg_wait(sample, cur_height, true)? {
                return Ok(PollStep::Done(MissedVoteOutcome::NeedsDkgWait(None)));
            }
            self.reset_window(cur_height, cur_vrf, end_missed);
            return Ok(PollStep::Continue(format!(
                "participation counters reset at epoch boundary before updating `{}`; retrying {} blocks from height {}",
                self.member.name, MISSED_VOTE_OBSERVATION_BLOCKS, self.start_height
            )));
        }

        let offenders = collect_missed_vote_offenders(&self.start_missed, &end_missed);
        if !offenders.is_empty() {
            let elapsed = cur_height.saturating_sub(self.start_height);
            let summary = format!(
                "{} over {elapsed} finalized blocks ({}..={cur_height})",
                offenders.join(", "),
                self.start_height
            );
            *last_offenders = Some(summary.clone());
            if self.needs_dkg_wait(sample, cur_height, true)? {
                return Ok(PollStep::Done(MissedVoteOutcome::NeedsDkgWait(Some(
                    format!(
                        "missed votes observed before updating `{}` ({summary}); deferring past next DKG boundary before retrying",
                        self.member.name
                    ),
                ))));
            }
            self.reset_window(cur_height, cur_vrf, end_missed);
            return Ok(PollStep::Continue(format!(
                "missed votes observed before updating `{}` ({summary}); retrying {} blocks from height {} (vrf={})",
                self.member.name, MISSED_VOTE_OBSERVATION_BLOCKS, self.start_height, self.start_vrf
            )));
        }

        let outcome = if self.needs_dkg_wait(sample, cur_height, false)? {
            MissedVoteOutcome::NeedsDkgWait(None)
        } else {
            MissedVoteOutcome::Clean
        };
        Ok(PollStep::Done(outcome))
    }
}

fn query_missed_votes_checked(
    ctx: &PollContext<'_>,
    probe: &DesiredMember,
    member: &DesiredMember,
    phase: &str,
) -> Result<BTreeMap<String, u64>> {
    query_validators_missed_votes(
        ctx.exec.as_ref(),
        &probe.vmm_host,
        &probe.name,
        probe.service_ip,
        probe.rpc_port,
    )
    .wrap_err_with(|| {
        format!(
            "refusing to stop `{}`: failed {phase} `IValidatorSet.validatorByAddress` check (fail-closed)",
            member.name
        )
    })
}

fn participation_counters_reset(
    start_missed: &BTreeMap<String, u64>,
    end_missed: &BTreeMap<String, u64>,
) -> bool {
    let any_decreased = start_missed
        .iter()
        .any(|(addr, &before)| end_missed.get(addr).is_some_and(|&after| after < before));
    let keys_changed = end_missed.len() != start_missed.len()
        || !start_missed.keys().all(|k| end_missed.contains_key(k));
    any_decreased || keys_changed
}

fn collect_missed_vote_offenders(
    start_missed: &BTreeMap<String, u64>,
    end_missed: &BTreeMap<String, u64>,
) -> Vec<String> {
    let mut offenders = Vec::new();
    for (addr, &before) in start_missed {
        let after = end_missed[addr];
        let delta = after.saturating_sub(before);
        if delta > 0 {
            offenders.push(format!("{addr}: +{delta} ({before} -> {after})"));
        }
    }
    offenders
}

struct DkgBoundaryCrossed {
    statuses: Vec<VmMemberStatus>,
    base_vrf: u64,
    cur_vrf: u64,
    min_finalized: u64,
}

pub(super) async fn wait_post_rollout_dkg_boundary(
    ctx: &PollContext<'_>,
    deadline: Instant,
    progress: &mut Progress,
) -> Result<Vec<VmMemberStatus>> {
    let mut boundary_target: Option<(u64, u64)> = None;

    let outcome = poll_network_until(ctx, deadline, progress, |sample| {
        if boundary_target.is_none() {
            boundary_target = init_post_rollout_dkg_target(sample)?;
        }
        if let Some((base_vrf, target_act)) = boundary_target {
            if let Some(done) = check_post_rollout_dkg_crossed(ctx, sample, base_vrf, target_act)? {
                return Ok(PollStep::Done(done));
            }
        }
        Ok(PollStep::Continue(format!(
            "waiting for DKG boundary: start_vrf={:?} cur_vrf={:?} min_finalized={:?} target_activation={:?}",
            boundary_target.map(|(v, _)| v),
            sample.common_vrf,
            sample.min_finalized(),
            boundary_target.map(|(_, a)| a),
        )))
    })
    .await?;

    let crossed = match outcome {
        Ok(res) => res,
        Err(sample) => {
            bail!(
                "timed out waiting for post-update DKG boundary crossing (start_vrf={:?}, target_activation={:?}):\n\n{}",
                boundary_target.map(|(v, _)| v),
                boundary_target.map(|(_, a)| a),
                render_vm_status(ctx.build, &sample.statuses, &sample.orphans, ctx.max_lag)
            );
        }
    };

    progress.update(&format!(
        "DKG boundary crossed in lockstep: vrf_material_version {} -> {} at min_finalized={}",
        crossed.base_vrf, crossed.cur_vrf, crossed.min_finalized
    ));
    Ok(crossed.statuses)
}

fn init_post_rollout_dkg_target(sample: &NetworkSample) -> Result<Option<(u64, u64)>> {
    let Some(vrf) = sample.common_vrf else {
        return Ok(None);
    };
    let next_act = sample
        .statuses
        .iter()
        .filter(|s| s.role != MemberRole::FullNode)
        .filter_map(|s| {
            s.consensus
                .as_ref()
                .map(|c| c.next_planned_activation_height)
        })
        .max()
        .unwrap_or(0);
    ensure!(
        next_act > 0,
        "cannot verify post-update DKG boundary crossing: `next_planned_activation_height` is 0 or missing in `outbe_consensusStatus`"
    );
    Ok(Some((vrf, next_act)))
}

fn check_post_rollout_dkg_crossed(
    ctx: &PollContext<'_>,
    sample: &NetworkSample,
    base_vrf: u64,
    target_act: u64,
) -> Result<Option<DkgBoundaryCrossed>> {
    let Some(cur_vrf) = sample.common_vrf else {
        return Ok(None);
    };
    let min_finalized = sample.min_finalized().unwrap_or(0);
    let crossed = sample.fully_healthy()
        && all_validators_randomness_healthy(&sample.statuses)
        && cur_vrf > base_vrf
        && min_finalized >= target_act;
    if !crossed {
        return Ok(None);
    }
    for i in 1..ctx.desired_members.len() {
        if !verify_finalized_hash_match(
            ctx.exec.as_ref(),
            &ctx.desired_members[0],
            &sample.statuses[0],
            &ctx.desired_members[i],
            &sample.statuses[i],
        )? {
            return Ok(None);
        }
    }
    Ok(Some(DkgBoundaryCrossed {
        statuses: sample.statuses.clone(),
        base_vrf,
        cur_vrf,
        min_finalized,
    }))
}
