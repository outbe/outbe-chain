//! Rolling guest OS image upgrade across a live VM network (`outbe-net-up network update`, S7).
//!
//! Strictly updates one VM at a time via graceful `outbe-up vm update` (`ShutdownVm` -> `UpdateVm`
//! -> `StartVm`, never `--force` / `StopVm`), preserving every VM's UUID, MAC, static IP, and
//! `hda.img` data disk.
//!
//! Safety guarantees:
//! - Resumable after an interrupted roll (SSH drop after host `vm update` succeeded, or graceful
//!   stop timeout leaving 1 VM stopped): accepts `{recorded image, target image}` per member,
//!   finishes the single interrupted member first, and refuses if `>= 2` members are unhealthy.
//! - Time-based, fail-closed DKG guard: measures block rate from 2 samples to size the guard window
//!   for `stop_timeout + boot_budget + margin` (floored by `--dkg-guard-blocks >= 1`), defers during
//!   active DKG ceremonies (`randomness_status != "healthy"`), and fails closed if consensus status
//!   or `next_planned_activation_height` is missing/zero.
//! - Fresh health re-check (`orphans.is_empty() && all_ready && common_vrf`) immediately before
//!   every VM stop so two validators can never go down simultaneously.
//! - Post-update lockstep check (`+min_finalized_blocks` across 2 consecutive samples + exact block
//!   hash match) after every VM, and mandatory post-rollout DKG boundary verification across all
//!   validators in lockstep (S7 DoD).

mod dkg;
mod health;
mod rollout;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::progress::Progress;
use outbe_operator_remote::{HostExec, SshControlSession};
use tokio::task::JoinSet;

use super::{
    build_all_desired_members, collect_unique_hosts, ensure_pinned_outbe_up_on_host_from_bundle,
    host_key, load_and_verify_vm_orchestration, member_node_ready, publish_image_on_host,
    render_vm_status, resolve_bundled_outbe_up_sha256_from_bundle, resolve_image_source,
    DesiredMember, HostImageTarget, ImageSource, MemberLagAssessment, OrphanVmStatus,
    RemoteImagePublishOutput, RemoteOutbeUp, RemoteVmStatusOutput, ResolvedImageSource, UniqueHost,
    VmMemberStatus,
};
use crate::network::{
    remote::NetworkArtifacts,
    vm_state::{MemberRole, NetworkVmState, NetworkVmStateLock, CURRENT_ENV_SCHEMA},
};

use dkg::{pre_stop_health_and_dkg_gate, wait_post_rollout_dkg_boundary};
use health::{
    common_validator_vrf_excluding, ensure_all_classified_ours, ensure_all_members_healthy,
    fetch_all_host_vms, find_observed_vm, measure_blocks_per_sec, require_observed_vm,
    sample_network, validator_required_peers,
};
use rollout::{
    record_member_target_image, run_remote_vm_update, wait_post_vm_update_lockstep, LockstepTarget,
    RemoteUpdateTarget,
};

pub(crate) use health::compute_effective_dkg_guard_blocks;
#[cfg(test)]
pub(crate) use health::{all_validators_randomness_healthy, check_validators_connected_peers};

#[derive(Clone, Debug)]
pub struct UpdateOptions<'a> {
    pub artifacts: &'a Path,
    pub image_source: ImageSource,
    /// Optional software bundle overriding `<artifacts>/software-bundle.tar.zst` when pinning
    /// `outbe-up` on hosts (e.g. upgrading a network built on an older bundle to an `outbe-up`
    /// that implements `vm update`). Recorded in `network-vm-state.json` (`outbe_up_sha256`) so
    /// subsequent `status`/`stop`/`start`/`remove` commands use the same pinned binary.
    pub software_bundle: Option<&'a Path>,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    /// Per-VM post-update recovery timeout (and post-rollout DKG boundary wait timeout).
    pub timeout: Duration,
    /// Graceful guest shutdown timeout passed to `outbe-up vm update --stop-timeout` (never forced).
    pub stop_timeout: Duration,
    pub max_lag: u64,
    pub min_finalized_blocks: u64,
    /// Minimum DKG guard window in finalized blocks (must be >= 1; actual guard also accounts for
    /// `stop_timeout + boot_budget` from the measured block rate).
    pub dkg_guard_blocks: u64,
}

#[derive(Clone)]
pub(super) struct TargetImage {
    pub(super) name: String,
    pub(super) hash: String,
}

pub(super) struct PollContext<'a> {
    pub(super) build: &'a NetworkArtifacts,
    pub(super) desired_members: &'a [DesiredMember],
    pub(super) unique_hosts: &'a BTreeMap<String, UniqueHost>,
    pub(super) host_bins: &'a BTreeMap<String, RemoteOutbeUp>,
    pub(super) exec: &'a Arc<dyn HostExec>,
    pub(super) required_peers: usize,
    pub(super) max_lag: u64,
}

pub(super) struct NetworkSample {
    pub(super) sampled_at: Instant,
    pub(super) statuses: Vec<VmMemberStatus>,
    pub(super) assessments: Vec<MemberLagAssessment>,
    pub(super) orphans: Vec<OrphanVmStatus>,
    pub(super) all_ready: bool,
    pub(super) common_vrf: Option<u64>,
    pub(super) has_validators: bool,
}

impl NetworkSample {
    pub(super) fn vrf_ok(&self) -> bool {
        !self.has_validators || self.common_vrf.is_some()
    }

    pub(super) fn fully_healthy(&self) -> bool {
        self.orphans.is_empty() && self.all_ready && self.vrf_ok()
    }

    pub(super) fn min_finalized(&self) -> Option<u64> {
        self.statuses.iter().filter_map(|s| s.finalized).min()
    }

    pub(super) fn max_finalized(&self) -> Option<u64> {
        self.statuses.iter().filter_map(|s| s.finalized).max()
    }

    pub(super) fn clone_light(&self) -> Self {
        Self {
            sampled_at: self.sampled_at,
            statuses: self.statuses.clone(),
            assessments: self.assessments.clone(),
            orphans: self.orphans.clone(),
            all_ready: self.all_ready,
            common_vrf: self.common_vrf,
            has_validators: self.has_validators,
        }
    }
}

pub(super) enum PollStep<T> {
    Done(T),
    Continue(String),
}

struct RolloutEnv<'a> {
    options: &'a UpdateOptions<'a>,
    build: &'a NetworkArtifacts,
    unique_hosts: &'a BTreeMap<String, UniqueHost>,
    host_bins: &'a BTreeMap<String, RemoteOutbeUp>,
    host_images: &'a BTreeMap<String, RemoteImagePublishOutput>,
    target_image: &'a TargetImage,
    exec: &'a Arc<dyn HostExec>,
    state_lock: &'a NetworkVmStateLock,
    required_peers: usize,
}

impl<'a> RolloutEnv<'a> {
    fn poll_ctx<'b>(&'b self, desired_members: &'b [DesiredMember]) -> PollContext<'b> {
        PollContext {
            build: self.build,
            desired_members,
            unique_hosts: self.unique_hosts,
            host_bins: self.host_bins,
            exec: self.exec,
            required_peers: self.required_peers,
            max_lag: self.options.max_lag,
        }
    }
}

pub async fn update(options: UpdateOptions<'_>) -> Result<()> {
    let build = NetworkArtifacts::load(options.artifacts)?;
    let session = Arc::new(SshControlSession::new_for_network(
        options.ssh_key,
        options.ssh_config,
        &build.manifest.genesis.final_sha256,
    )?);
    let exec: Arc<dyn HostExec> = session.clone();
    let res = update_with_exec(&options, exec).await;
    session.close_all();
    res
}

pub(crate) async fn update_with_exec(
    options: &UpdateOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    ensure!(
        options.dkg_guard_blocks > 0,
        "refusing `--dkg-guard-blocks 0`: disabling the DKG boundary guard is not permitted"
    );

    let mut progress = Progress::new();
    progress.stage("verifying network artifacts, software bundle, and target guest OS image");
    let build =
        load_and_verify_vm_orchestration(options.artifacts, exec.as_ref(), &mut progress).await?;

    let bundle_path = options
        .software_bundle
        .map(Path::to_path_buf)
        .unwrap_or_else(|| build.root.join("software-bundle.tar.zst"));
    let outbe_up_sha256 = resolve_bundled_outbe_up_sha256_from_bundle(&bundle_path)?;
    let (resolved_image, image_source_id) = resolve_image_source(&options.image_source)?;

    let state_lock = NetworkVmState::lock(options.artifacts)?;
    let mut state = load_update_state(&state_lock, options.artifacts, &build)?;
    let mut desired_members = build_all_desired_members(&build, Some(&state), None, None, None)?;
    ensure!(
        !desired_members.is_empty(),
        "network `{}` has no VM members to update",
        build.config.name
    );
    let unique_hosts = collect_unique_hosts(&desired_members);

    let (host_bins, host_images, target_image) = pin_and_publish_target_image(
        &mut progress,
        &exec,
        (
            &unique_hosts,
            &bundle_path,
            &outbe_up_sha256,
            &resolved_image,
        ),
        (&mut state, &state_lock, options.artifacts),
    )
    .await?;

    progress.stage("verifying pre-update network health across all members");
    let mut host_vms_map = fetch_all_host_vms(&unique_hosts, &host_bins, exec.as_ref())?;
    mark_members_already_on_target(&mut desired_members, &host_vms_map, &target_image);

    let required_peers = validator_required_peers(&desired_members);
    let env = RolloutEnv {
        options,
        build: &build,
        unique_hosts: &unique_hosts,
        host_bins: &host_bins,
        host_images: &host_images,
        target_image: &target_image,
        exec: &exec,
        state_lock: &state_lock,
        required_peers,
    };

    let mut updated_count = 0usize;
    let mut resumed_indices = BTreeSet::new();
    let pre_sample = preflight_and_resume_if_needed(
        &env,
        &mut desired_members,
        &mut host_vms_map,
        (&mut state, &mut updated_count, &mut resumed_indices),
        &mut progress,
    )
    .await?;

    let skipped_count = roll_remaining_members(
        &env,
        &mut desired_members,
        &host_vms_map,
        (
            &mut state,
            &pre_sample,
            &resumed_indices,
            &mut updated_count,
        ),
        &mut progress,
    )
    .await?;

    state.image_name = target_image.name.clone();
    state.image_hash = target_image.hash.clone();
    state.image_source_id = image_source_id;
    state.outbe_up_sha256 = outbe_up_sha256;
    state.save_locked(&state_lock, options.artifacts)?;
    drop(state_lock);

    let ctx = PollContext {
        build: &build,
        desired_members: &desired_members,
        unique_hosts: &unique_hosts,
        host_bins: &host_bins,
        exec: &exec,
        required_peers,
        max_lag: options.max_lag,
    };
    let final_statuses = if desired_members
        .iter()
        .any(|m| m.role != MemberRole::FullNode)
    {
        progress
            .stage("verifying post-update DKG boundary crossing across all validators in lockstep");
        wait_post_rollout_dkg_boundary(&ctx, Instant::now() + options.timeout, &mut progress)
            .await?
    } else {
        sample_network(&ctx).await?.statuses
    };

    progress.done(&format!(
        "rolling update complete for `{}`: {updated_count} updated, {skipped_count} already on `{}` ({})",
        build.config.name,
        target_image.name,
        &target_image.hash[..target_image.hash.len().min(16)]
    ));
    println!(
        "\n{}",
        render_vm_status(&build, &final_statuses, &[], options.max_lag)
    );
    Ok(())
}

fn load_update_state(
    state_lock: &NetworkVmStateLock,
    artifacts: &Path,
    build: &NetworkArtifacts,
) -> Result<NetworkVmState> {
    let state = NetworkVmState::load_optional_locked(state_lock, artifacts)?.ok_or_else(|| {
        eyre::eyre!(
            "network-vm-state.json not found in {}; deploy and start the network before running `network update`",
            artifacts.display()
        )
    })?;
    state.ensure_matches_network(build)?;
    ensure!(
        state.env_schema >= CURRENT_ENV_SCHEMA,
        "refusing rolling update: existing network disks use validator.env schema v{} \
         (missing OUTBE_VM_IP / OUTBE_RPC_PORT / OUTBE_RETH_P2P_PORT), but this outbe-net-up \
         requires schema v{CURRENT_ENV_SCHEMA}; images built from this revision only boot freshly prepared v{CURRENT_ENV_SCHEMA} \
         disks (or migrate validator.env on member disks before updating)",
        state.env_schema,
    );
    Ok(state)
}

async fn pin_and_publish_target_image(
    progress: &mut Progress,
    exec: &Arc<dyn HostExec>,
    (unique_hosts, bundle_path, outbe_up_sha256, resolved_image): (
        &BTreeMap<String, UniqueHost>,
        &PathBuf,
        &str,
        &ResolvedImageSource,
    ),
    (state, state_lock, artifacts): (&mut NetworkVmState, &NetworkVmStateLock, &Path),
) -> Result<(
    BTreeMap<String, RemoteOutbeUp>,
    BTreeMap<String, RemoteImagePublishOutput>,
    TargetImage,
)> {
    progress.stage(&format!(
        "pinning outbe-up ({}) and publishing target image on {} host(s)",
        &outbe_up_sha256[..16],
        unique_hosts.len()
    ));
    let mut host_bins = BTreeMap::new();
    for (hkey, host) in unique_hosts {
        let bin = ensure_pinned_outbe_up_on_host_from_bundle(
            exec.as_ref(),
            &host.ssh,
            bundle_path,
            outbe_up_sha256,
            true,
        )?;
        host_bins.insert(hkey.clone(), bin);
    }
    if state.outbe_up_sha256 != outbe_up_sha256 {
        state.outbe_up_sha256 = outbe_up_sha256.to_owned();
        state.save_locked(state_lock, artifacts)?;
    }

    let mut img_tasks = JoinSet::new();
    for (hkey, host) in unique_hosts {
        let exec = Arc::clone(exec);
        let hkey = hkey.clone();
        let ssh = host.ssh.clone();
        let bin = host_bins[&hkey].clone();
        let vmm_home = host.vmm_home.clone();
        let resolved = resolved_image.clone();
        img_tasks.spawn_blocking(move || {
            let target = HostImageTarget {
                ssh: &ssh,
                bin: &bin,
                vmm_home: &vmm_home,
            };
            let res = publish_image_on_host(exec.as_ref(), &target, &resolved);
            (hkey, res)
        });
    }
    let mut host_images = BTreeMap::new();
    while let Some(joined) = img_tasks.join_next().await {
        let (hkey, res) = joined.wrap_err("image publish task panicked")?;
        host_images.insert(hkey, res?);
    }

    let first_img = host_images
        .values()
        .next()
        .ok_or_else(|| eyre::eyre!("no target hosts resolved for image publish"))?;
    let target_image = TargetImage {
        name: first_img.image_name.clone(),
        hash: first_img.os_image_hash.clone(),
    };
    for (hkey, img) in &host_images {
        ensure!(
            img.image_name == target_image.name && img.os_image_hash == target_image.hash,
            "host {hkey} published image `{}` ({}), expected `{}` ({})",
            img.image_name,
            img.os_image_hash,
            target_image.name,
            target_image.hash
        );
    }
    Ok((host_bins, host_images, target_image))
}

fn mark_members_already_on_target(
    desired_members: &mut [DesiredMember],
    host_vms_map: &BTreeMap<String, Vec<RemoteVmStatusOutput>>,
    target_image: &TargetImage,
) {
    for member in desired_members {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        if let Some(obs) = find_observed_vm(host_vms_map, &hkey, &member.vm_name) {
            if obs.image_name == target_image.name && obs.os_image_hash == target_image.hash {
                member.image_name = Some(target_image.name.clone());
                member.image_hash = Some(target_image.hash.clone());
            }
        }
    }
}

async fn preflight_and_resume_if_needed(
    env: &RolloutEnv<'_>,
    desired_members: &mut [DesiredMember],
    host_vms_map: &mut BTreeMap<String, Vec<RemoteVmStatusOutput>>,
    (state, updated_count, resumed_indices): (
        &mut NetworkVmState,
        &mut usize,
        &mut BTreeSet<usize>,
    ),
    progress: &mut Progress,
) -> Result<NetworkSample> {
    let mut pre_sample = sample_network(&env.poll_ctx(desired_members)).await?;
    ensure!(
        pre_sample.orphans.is_empty(),
        "refusing rolling update: found {} orphan VM(s) with this network's owner_tag prefix",
        pre_sample.orphans.len()
    );
    ensure_all_classified_ours(
        env.build,
        &pre_sample,
        env.options.max_lag,
        "pre-update check",
    )?;

    if !pre_sample.fully_healthy() {
        if let Some(resumed) = try_resume_single_unhealthy_member(
            env,
            desired_members,
            host_vms_map,
            (&pre_sample, state, updated_count, resumed_indices),
            progress,
        )
        .await?
        {
            pre_sample = resumed;
        }
    }

    ensure!(
        pre_sample.orphans.is_empty(),
        "refusing rolling update: found {} orphan VM(s) with this network's owner_tag prefix",
        pre_sample.orphans.len()
    );
    ensure_all_members_healthy(
        env.build,
        &pre_sample,
        env.required_peers,
        env.options.max_lag,
        "pre-update health check",
    )?;
    Ok(pre_sample)
}

async fn try_resume_single_unhealthy_member(
    env: &RolloutEnv<'_>,
    desired_members: &mut [DesiredMember],
    host_vms_map: &mut BTreeMap<String, Vec<RemoteVmStatusOutput>>,
    (pre_sample, state, updated_count, resumed_indices): (
        &NetworkSample,
        &mut NetworkVmState,
        &mut usize,
        &mut BTreeSet<usize>,
    ),
    progress: &mut Progress,
) -> Result<Option<NetworkSample>> {
    let degraded_required = env.required_peers.saturating_sub(1);
    let unhealthy_indices: Vec<usize> = pre_sample
        .statuses
        .iter()
        .zip(&pre_sample.assessments)
        .enumerate()
        .filter(|(_, (s, a))| {
            !member_node_ready(s, env.build.config.chain_id, degraded_required) || a.lagging
        })
        .map(|(i, _)| i)
        .collect();

    if unhealthy_indices.len() >= 2 {
        let names: Vec<&str> = unhealthy_indices
            .iter()
            .map(|&i| pre_sample.statuses[i].name.as_str())
            .collect();
        bail!(
            "refusing rolling update: found {} unhealthy member(s) ({}) — at most 1 interrupted member may be resumed\n\n{}",
            unhealthy_indices.len(),
            names.join(", "),
            render_vm_status(env.build, &pre_sample.statuses, &[], env.options.max_lag)
        );
    }

    let &[u] = unhealthy_indices.as_slice() else {
        return Ok(None);
    };
    let member = desired_members[u].clone();
    let hkey = host_key(&member.vmm_host, &member.vmm_home);
    let observed = require_observed_vm(host_vms_map, &hkey, &member)?;
    let on_target_image = observed.image_name == env.target_image.name
        && observed.os_image_hash == env.target_image.hash;
    let remaining_vrf_ok = common_validator_vrf_excluding(&pre_sample.statuses, Some(u)).is_some();
    if (observed.running && !on_target_image) || !remaining_vrf_ok {
        return Ok(None);
    }

    progress.stage(&format!(
        "resuming interrupted member `{}` ({}) before continuing rolling update (running={}, on_target_image={on_target_image})",
        member.name, observed.vm_id, observed.running
    ));
    let per_vm_deadline = Instant::now() + env.options.timeout;
    if !observed.running || !on_target_image {
        let updated = run_remote_vm_update(
            env.exec.as_ref(),
            &RemoteUpdateTarget {
                member: &member,
                observed: &observed,
                bin: &env.host_bins[&hkey],
                host_img: &env.host_images[&hkey],
            },
            env.target_image,
            env.options.stop_timeout,
        )?;
        if !updated.already_updated {
            *updated_count += 1;
        }
    }
    record_member_target_image(
        &mut desired_members[u],
        (state, env.state_lock, env.options.artifacts),
        &observed,
        env.target_image,
    )?;
    resumed_indices.insert(u);

    let ctx = env.poll_ctx(desired_members);
    wait_post_vm_update_lockstep(
        &ctx,
        &LockstepTarget {
            updated_idx: u,
            updated_vm_id: &observed.vm_id,
            min_finalized_blocks: env.options.min_finalized_blocks,
        },
        per_vm_deadline,
        progress,
    )
    .await?;

    *host_vms_map = fetch_all_host_vms(env.unique_hosts, env.host_bins, env.exec.as_ref())?;
    Ok(Some(sample_network(&ctx).await?))
}

async fn roll_remaining_members(
    env: &RolloutEnv<'_>,
    desired_members: &mut [DesiredMember],
    host_vms_map: &BTreeMap<String, Vec<RemoteVmStatusOutput>>,
    (state, pre_sample, resumed_indices, updated_count): (
        &mut NetworkVmState,
        &NetworkSample,
        &BTreeSet<usize>,
        &mut usize,
    ),
    progress: &mut Progress,
) -> Result<usize> {
    let total = desired_members.len();
    let mut skipped_count = 0usize;
    let mut measured_bps: Option<f64> = None;

    for idx in 0..total {
        if resumed_indices.contains(&idx) {
            continue;
        }
        let member = desired_members[idx].clone();
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let observed = require_observed_vm(host_vms_map, &hkey, &member)?;

        if observed.os_image_hash == env.target_image.hash
            && observed.image_name == env.target_image.name
        {
            progress.update(&format!(
                "[{}/{}] `{}` ({}) already running target image `{}`; skipping",
                idx + 1,
                total,
                member.name,
                observed.vm_id,
                env.target_image.name
            ));
            record_member_target_image(
                &mut desired_members[idx],
                (state, env.state_lock, env.options.artifacts),
                &observed,
                env.target_image,
            )?;
            skipped_count += 1;
            continue;
        }

        let per_vm_deadline = Instant::now() + env.options.timeout;
        let effective_dkg_guard =
            resolve_member_dkg_guard(env, desired_members, pre_sample, &member, &mut measured_bps)
                .await?;

        pre_stop_health_and_dkg_gate(
            &env.poll_ctx(desired_members),
            &member,
            effective_dkg_guard,
            per_vm_deadline,
            progress,
        )
        .await?;

        progress.stage(&format!(
            "[{}/{}] gracefully updating `{}` ({}) on {} to `{}`",
            idx + 1,
            total,
            member.name,
            observed.vm_id,
            member.vmm_host.host,
            env.target_image.name
        ));
        let updated = run_remote_vm_update(
            env.exec.as_ref(),
            &RemoteUpdateTarget {
                member: &member,
                observed: &observed,
                bin: &env.host_bins[&hkey],
                host_img: &env.host_images[&hkey],
            },
            env.target_image,
            env.options.stop_timeout,
        )?;
        record_member_target_image(
            &mut desired_members[idx],
            (state, env.state_lock, env.options.artifacts),
            &observed,
            env.target_image,
        )?;
        if !updated.already_updated {
            *updated_count += 1;
        }

        progress.stage(&format!(
            "[{}/{}] waiting for `{}` recovery and +{} finalized blocks in lockstep",
            idx + 1,
            total,
            member.name,
            env.options.min_finalized_blocks
        ));
        wait_post_vm_update_lockstep(
            &env.poll_ctx(desired_members),
            &LockstepTarget {
                updated_idx: idx,
                updated_vm_id: &observed.vm_id,
                min_finalized_blocks: env.options.min_finalized_blocks,
            },
            per_vm_deadline,
            progress,
        )
        .await?;
    }
    Ok(skipped_count)
}

async fn resolve_member_dkg_guard(
    env: &RolloutEnv<'_>,
    desired_members: &[DesiredMember],
    pre_sample: &NetworkSample,
    member: &DesiredMember,
    measured_bps: &mut Option<f64>,
) -> Result<u64> {
    if member.role == MemberRole::FullNode {
        return Ok(env.options.dkg_guard_blocks);
    }
    let bps = match *measured_bps {
        Some(b) => b,
        None => {
            let b = measure_blocks_per_sec(&env.poll_ctx(desired_members), pre_sample).await?;
            *measured_bps = Some(b);
            b
        }
    };
    Ok(compute_effective_dkg_guard_blocks(
        env.options.stop_timeout,
        env.options.dkg_guard_blocks,
        bps,
    ))
}
