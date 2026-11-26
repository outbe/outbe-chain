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

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::progress::Progress;
use outbe_operator_remote::{run_host_json, shell_quote, HostExec, SshControlSession};
use tokio::task::JoinSet;

use super::{
    assess_members_lag, build_all_desired_members, build_member_state, collect_unique_hosts,
    compute_readiness_baseline, ensure_pinned_outbe_up_on_host_from_bundle, ensure_vm_deployment,
    fetch_host_vms, fetch_image_on_host, host_key, import_image_on_host, member_node_ready,
    members::{fetch_member_serial_tail, verify_finalized_hash_match},
    query_members_and_orphans, query_validators_missed_votes, render_vm_status,
    resolve_bundled_outbe_up_sha256_from_bundle, resolve_image_source, shell_quote_tilde_path,
    DesiredMember, ImageSource, MemberLagAssessment, OrphanVmStatus, RemoteImagePublishOutput,
    RemoteOutbeUp, RemoteVmStatusOutput, RemoteVmUpdateOutput, ResolvedImageSource, UniqueHost,
    VmClassKind, VmMemberStatus, READY_CONSECUTIVE_SAMPLES,
};
use crate::network::{
    remote::NetworkArtifacts,
    vm_state::{MemberRole, NetworkVmState},
};

#[cfg(not(test))]
const UPDATE_POLL_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(test)]
const UPDATE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(not(test))]
const BLOCK_RATE_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
#[cfg(test)]
const BLOCK_RATE_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);

/// Conservative budget for guest boot + `outbe-vm-agent vm-boot` + TEE re-admission (`registerEnclave`)
/// + catch-up added to `stop_timeout` when computing the time-based DKG guard window.
const DKG_BOOT_AND_TEE_BUDGET_SECS: f64 = 60.0;
const DKG_GUARD_MARGIN_BLOCKS: u64 = 15;
/// Forward finalized-block observation window for verifying zero missed votes across all
/// validators (`IValidatorSet.validatorByAddress`) before stopping any VM.
const MISSED_VOTE_OBSERVATION_BLOCKS: u64 = 60;

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

struct PollContext<'a> {
    build: &'a NetworkArtifacts,
    desired_members: &'a [DesiredMember],
    unique_hosts: &'a BTreeMap<String, UniqueHost>,
    host_bins: &'a BTreeMap<String, RemoteOutbeUp>,
    exec: &'a Arc<dyn HostExec>,
    required_peers: usize,
    max_lag: u64,
}

struct NetworkSample {
    sampled_at: Instant,
    statuses: Vec<VmMemberStatus>,
    assessments: Vec<MemberLagAssessment>,
    orphans: Vec<OrphanVmStatus>,
    all_ready: bool,
    common_vrf: Option<u64>,
    has_validators: bool,
}

impl NetworkSample {
    fn vrf_ok(&self) -> bool {
        !self.has_validators || self.common_vrf.is_some()
    }

    fn fully_healthy(&self) -> bool {
        self.orphans.is_empty() && self.all_ready && self.vrf_ok()
    }

    fn min_finalized(&self) -> Option<u64> {
        self.statuses.iter().filter_map(|s| s.finalized).min()
    }

    fn max_finalized(&self) -> Option<u64> {
        self.statuses.iter().filter_map(|s| s.finalized).max()
    }

    fn clone_light(&self) -> Self {
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

enum PollStep<T> {
    Done(T),
    Continue(String),
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

    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;
    super::ensure_vm_rpc_reachable_for_orchestration(&build)?;
    let first_host = build
        .config
        .resolve_founder_vm(&build.config.founders[0])?
        .vmm_host;
    progress.stage(&format!(
        "verifying certified_rpc ({}) answers eth_chainId == {}",
        build.config.certified_rpc, build.config.chain_id
    ));
    super::verify_certified_rpc_chain_id(&build, &first_host, exec.as_ref()).await?;

    let bundle_path = options
        .software_bundle
        .map(Path::to_path_buf)
        .unwrap_or_else(|| build.root.join("software-bundle.tar.zst"));
    let outbe_up_sha256 = resolve_bundled_outbe_up_sha256_from_bundle(&bundle_path)?;
    let (resolved_image, image_source_id) = resolve_image_source(&options.image_source)?;

    let state_lock = NetworkVmState::lock(options.artifacts)?;
    let mut state = NetworkVmState::load_optional_locked(&state_lock, options.artifacts)?
        .ok_or_else(|| {
            eyre::eyre!(
                "network-vm-state.json not found in {}; deploy and start the network before running `network update`",
                options.artifacts.display()
            )
        })?;
    state.ensure_matches_network(&build)?;
    ensure!(
        state.env_schema >= crate::network::vm_state::CURRENT_ENV_SCHEMA,
        "refusing rolling update: existing network disks use validator.env schema v{} \
         (missing OUTBE_VM_IP / OUTBE_RPC_PORT / OUTBE_RETH_P2P_PORT), but this outbe-net-up \
         requires schema v{}; images built from this revision only boot freshly prepared v{} \
         disks (or migrate validator.env on member disks before updating)",
        state.env_schema,
        crate::network::vm_state::CURRENT_ENV_SCHEMA,
        crate::network::vm_state::CURRENT_ENV_SCHEMA,
    );

    let mut desired_members = build_all_desired_members(&build, Some(&state), None, None, None)?;
    ensure!(
        !desired_members.is_empty(),
        "network `{}` has no VM members to update",
        build.config.name
    );
    let unique_hosts = collect_unique_hosts(&desired_members);

    // 1. Pin `outbe-up` on every host and ensure target image is published/verified in parallel.
    progress.stage(&format!(
        "pinning outbe-up ({}) and publishing target image on {} host(s)",
        &outbe_up_sha256[..16],
        unique_hosts.len()
    ));
    let mut host_bins: BTreeMap<String, RemoteOutbeUp> = BTreeMap::new();
    for (hkey, host) in &unique_hosts {
        let bin = ensure_pinned_outbe_up_on_host_from_bundle(
            exec.as_ref(),
            &host.ssh,
            &bundle_path,
            &outbe_up_sha256,
            true,
        )?;
        host_bins.insert(hkey.clone(), bin);
    }

    // Persist the newly pinned `outbe_up_sha256` in state once every host has it installed so
    // subsequent `status`/`stop`/`start`/`remove` commands use the same pinned binary.
    if state.outbe_up_sha256 != outbe_up_sha256 {
        state.outbe_up_sha256 = outbe_up_sha256.clone();
        state.save_locked(&state_lock, options.artifacts)?;
    }

    let mut img_tasks = JoinSet::new();
    for (hkey, host) in &unique_hosts {
        let exec = Arc::clone(&exec);
        let hkey = hkey.clone();
        let ssh = host.ssh.clone();
        let bin = host_bins[&hkey].clone();
        let vmm_home = host.vmm_home.clone();
        let resolved = resolved_image.clone();
        img_tasks.spawn_blocking(move || {
            let res = match resolved {
                ResolvedImageSource::Archive { path, sha256 } => {
                    import_image_on_host(exec.as_ref(), &ssh, &bin, &vmm_home, &path, &sha256)
                }
                ResolvedImageSource::Url { url, sha256 } => {
                    fetch_image_on_host(exec.as_ref(), &ssh, &bin, &vmm_home, &url, &sha256)
                }
            };
            (hkey, res)
        });
    }
    let mut host_images: BTreeMap<String, RemoteImagePublishOutput> = BTreeMap::new();
    while let Some(joined) = img_tasks.join_next().await {
        let (hkey, res) = joined.wrap_err("image publish task panicked")?;
        host_images.insert(hkey, res?);
    }

    let first_img = host_images
        .values()
        .next()
        .ok_or_else(|| eyre::eyre!("no target hosts resolved for image publish"))?;
    let target_image_name = first_img.image_name.clone();
    let target_image_hash = first_img.os_image_hash.clone();
    for (hkey, img) in &host_images {
        ensure!(
            img.image_name == target_image_name && img.os_image_hash == target_image_hash,
            "host {hkey} published image `{}` ({}), expected `{target_image_name}` ({target_image_hash})",
            img.image_name,
            img.os_image_hash
        );
    }

    // 2. Resumable pre-flight (F1):
    //    - Fetch host VMM state and accept `{recorded image, target image}` per member so a member
    //      that already switched to the target image before an SSH drop is NOT rejected as `Stale`.
    //    - Refuse if any orphan exists, if any member is not `Ours`, or if `>= 2` members are
    //      unhealthy.
    //    - If exactly 1 member was interrupted mid-update (stopped, or on target image while still
    //      recovering), finish that single member first and wait for full lockstep before rolling
    //      the rest.
    progress.stage("verifying pre-update network health across all members");
    let mut host_vms_map = fetch_all_host_vms(&unique_hosts, &host_bins, exec.as_ref())?;
    for member in &mut desired_members {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        if let Some(obs) = find_observed_vm(&host_vms_map, &hkey, &member.vm_name) {
            if obs.image_name == target_image_name && obs.os_image_hash == target_image_hash {
                member.image_name = Some(target_image_name.clone());
                member.image_hash = Some(target_image_hash.clone());
            }
        }
    }

    let required_peers = validator_required_peers(&desired_members);
    let mut updated_count = 0usize;
    let mut skipped_count = 0usize;
    let mut resumed_indices: BTreeSet<usize> = BTreeSet::new();

    let mut pre_sample = {
        let ctx = PollContext {
            build: &build,
            desired_members: &desired_members,
            unique_hosts: &unique_hosts,
            host_bins: &host_bins,
            exec: &exec,
            required_peers,
            max_lag: options.max_lag,
        };
        sample_network(&ctx).await?
    };
    ensure!(
        pre_sample.orphans.is_empty(),
        "refusing rolling update: found {} orphan VM(s) with this network's owner_tag prefix",
        pre_sample.orphans.len()
    );
    ensure_all_classified_ours(&build, &pre_sample, options.max_lag, "pre-update check")?;

    if !pre_sample.fully_healthy() {
        let degraded_required = required_peers.saturating_sub(1);
        let unhealthy_indices: Vec<usize> = pre_sample
            .statuses
            .iter()
            .zip(&pre_sample.assessments)
            .enumerate()
            .filter(|(_, (s, a))| {
                !member_node_ready(s, build.config.chain_id, degraded_required) || a.lagging
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
                render_vm_status(&build, &pre_sample.statuses, &[], options.max_lag)
            );
        }

        if unhealthy_indices.len() == 1 {
            let u = unhealthy_indices[0];
            let member = desired_members[u].clone();
            let hkey = host_key(&member.vmm_host, &member.vmm_home);
            let observed = require_observed_vm(&host_vms_map, &hkey, &member)?;
            let on_target_image = observed.image_name == target_image_name
                && observed.os_image_hash == target_image_hash;
            let remaining_vrf_ok =
                common_validator_vrf_excluding(&pre_sample.statuses, Some(u)).is_some();

            if (!observed.running || on_target_image) && remaining_vrf_ok {
                progress.stage(&format!(
                    "resuming interrupted member `{}` ({}) before continuing rolling update (running={}, on_target_image={on_target_image})",
                    member.name, observed.vm_id, observed.running
                ));
                let per_vm_deadline = Instant::now() + options.timeout;
                if !observed.running || !on_target_image {
                    let updated = run_remote_vm_update(
                        exec.as_ref(),
                        &member,
                        &observed,
                        &host_bins[&hkey],
                        &host_images[&hkey],
                        &target_image_name,
                        &target_image_hash,
                        options.stop_timeout,
                    )?;
                    if !updated.already_updated {
                        updated_count += 1;
                    }
                }
                record_member_target_image(
                    &mut desired_members[u],
                    &mut state,
                    &state_lock,
                    options.artifacts,
                    &observed,
                    &target_image_name,
                    &target_image_hash,
                )?;
                resumed_indices.insert(u);

                let ctx = PollContext {
                    build: &build,
                    desired_members: &desired_members,
                    unique_hosts: &unique_hosts,
                    host_bins: &host_bins,
                    exec: &exec,
                    required_peers,
                    max_lag: options.max_lag,
                };
                wait_post_vm_update_lockstep(
                    &ctx,
                    u,
                    &observed.vm_id,
                    options.min_finalized_blocks,
                    per_vm_deadline,
                    &mut progress,
                )
                .await?;

                host_vms_map = fetch_all_host_vms(&unique_hosts, &host_bins, exec.as_ref())?;
                pre_sample = sample_network(&ctx).await?;
            }
        }
    }

    ensure!(
        pre_sample.orphans.is_empty(),
        "refusing rolling update: found {} orphan VM(s) with this network's owner_tag prefix",
        pre_sample.orphans.len()
    );
    ensure_all_members_healthy(
        &build,
        &pre_sample,
        required_peers,
        options.max_lag,
        "pre-update health check",
    )?;

    // 3. Rolling update: strictly ONE VM at a time.
    let total = desired_members.len();
    let mut measured_bps: Option<f64> = None;

    for idx in 0..total {
        if resumed_indices.contains(&idx) {
            continue;
        }
        let member = desired_members[idx].clone();
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let observed = require_observed_vm(&host_vms_map, &hkey, &member)?;

        if observed.os_image_hash == target_image_hash && observed.image_name == target_image_name {
            progress.update(&format!(
                "[{}/{}] `{}` ({}) already running target image `{target_image_name}`; skipping",
                idx + 1,
                total,
                member.name,
                observed.vm_id
            ));
            record_member_target_image(
                &mut desired_members[idx],
                &mut state,
                &state_lock,
                options.artifacts,
                &observed,
                &target_image_name,
                &target_image_hash,
            )?;
            skipped_count += 1;
            continue;
        }

        let per_vm_deadline = Instant::now() + options.timeout;

        // Measure block production rate once from 2 samples before the first validator stop (F2).
        let effective_dkg_guard = if member.role != MemberRole::FullNode {
            let bps = match measured_bps {
                Some(b) => b,
                None => {
                    let ctx = PollContext {
                        build: &build,
                        desired_members: &desired_members,
                        unique_hosts: &unique_hosts,
                        host_bins: &host_bins,
                        exec: &exec,
                        required_peers,
                        max_lag: options.max_lag,
                    };
                    let b = measure_blocks_per_sec(&ctx, &pre_sample).await?;
                    measured_bps = Some(b);
                    b
                }
            };
            compute_effective_dkg_guard_blocks(options.stop_timeout, options.dkg_guard_blocks, bps)
        } else {
            options.dkg_guard_blocks
        };

        // Fresh health re-check (F4) + time-based fail-closed DKG guard (F2) immediately before
        // stopping THIS VM (validators AND full nodes).
        {
            let ctx = PollContext {
                build: &build,
                desired_members: &desired_members,
                unique_hosts: &unique_hosts,
                host_bins: &host_bins,
                exec: &exec,
                required_peers,
                max_lag: options.max_lag,
            };
            pre_stop_health_and_dkg_gate(
                &ctx,
                &member,
                effective_dkg_guard,
                per_vm_deadline,
                &mut progress,
            )
            .await?;
        }

        // Graceful single-VM update (`ShutdownVm` -> `UpdateVm` -> `StartVm`; NEVER `--force`).
        progress.stage(&format!(
            "[{}/{}] gracefully updating `{}` ({}) on {} to `{target_image_name}`",
            idx + 1,
            total,
            member.name,
            observed.vm_id,
            member.vmm_host.host
        ));
        let updated = run_remote_vm_update(
            exec.as_ref(),
            &member,
            &observed,
            &host_bins[&hkey],
            &host_images[&hkey],
            &target_image_name,
            &target_image_hash,
            options.stop_timeout,
        )?;

        // Record the updated member image atomically so status & resume reflect progress immediately.
        record_member_target_image(
            &mut desired_members[idx],
            &mut state,
            &state_lock,
            options.artifacts,
            &observed,
            &target_image_name,
            &target_image_hash,
        )?;

        if !updated.already_updated {
            updated_count += 1;
        }

        // Wait for full network health, zero lag, finalized growth, and exact block hash match
        // BEFORE proceeding to the next VM.
        progress.stage(&format!(
            "[{}/{}] waiting for `{}` recovery and +{} finalized blocks in lockstep",
            idx + 1,
            total,
            member.name,
            options.min_finalized_blocks
        ));
        let ctx = PollContext {
            build: &build,
            desired_members: &desired_members,
            unique_hosts: &unique_hosts,
            host_bins: &host_bins,
            exec: &exec,
            required_peers,
            max_lag: options.max_lag,
        };
        wait_post_vm_update_lockstep(
            &ctx,
            idx,
            &observed.vm_id,
            options.min_finalized_blocks,
            per_vm_deadline,
            &mut progress,
        )
        .await?;
    }

    // 4. Commit network-wide target image metadata in `network-vm-state.json`.
    state.image_name = target_image_name.clone();
    state.image_hash = target_image_hash.clone();
    state.image_source_id = image_source_id;
    state.outbe_up_sha256 = outbe_up_sha256;
    state.save_locked(&state_lock, options.artifacts)?;
    drop(state_lock);

    // 5. Mandatory post-rollout DKG boundary verification (S7 DoD): wait for all validators to
    //    cross the next DKG activation boundary together in lockstep with matching finalized hash.
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
        "rolling update complete for `{}`: {updated_count} updated, {skipped_count} already on `{target_image_name}` ({})",
        build.config.name,
        &target_image_hash[..target_image_hash.len().min(16)]
    ));
    println!(
        "\n{}",
        render_vm_status(&build, &final_statuses, &[], options.max_lag)
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared network sampling & single polling loop
// ---------------------------------------------------------------------------

async fn sample_network(ctx: &PollContext<'_>) -> Result<NetworkSample> {
    let (statuses, orphans) = query_members_and_orphans(
        ctx.build,
        ctx.desired_members,
        ctx.unique_hosts,
        ctx.host_bins,
        ctx.exec,
    )
    .await?;
    let sampled_at = Instant::now();
    let assessments = assess_members_lag(&statuses, ctx.max_lag);
    let all_ready = statuses.iter().zip(&assessments).all(|(s, a)| {
        s.classification == VmClassKind::Ours
            && member_node_ready(s, ctx.build.config.chain_id, ctx.required_peers)
            && !a.lagging
    });
    let has_validators = statuses.iter().any(|s| s.role != MemberRole::FullNode);
    let common_vrf = common_validator_vrf(&statuses);
    Ok(NetworkSample {
        sampled_at,
        statuses,
        assessments,
        orphans,
        all_ready,
        common_vrf,
        has_validators,
    })
}

async fn poll_network_until<T, F>(
    ctx: &PollContext<'_>,
    deadline: Instant,
    progress: &mut Progress,
    mut step: F,
) -> Result<std::result::Result<T, NetworkSample>>
where
    F: FnMut(&NetworkSample) -> Result<PollStep<T>>,
{
    loop {
        let sample = sample_network(ctx).await?;
        match step(&sample)? {
            PollStep::Done(val) => return Ok(Ok(val)),
            PollStep::Continue(status_line) => {
                if Instant::now() >= deadline {
                    return Ok(Err(sample));
                }
                if !status_line.is_empty() {
                    progress.update(&status_line);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !remaining.is_zero() {
                    tokio::time::sleep(remaining.min(UPDATE_POLL_INTERVAL)).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Host VM lookup, remote update, and state recording helpers
// ---------------------------------------------------------------------------

fn fetch_all_host_vms(
    unique_hosts: &BTreeMap<String, UniqueHost>,
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    exec: &dyn HostExec,
) -> Result<BTreeMap<String, Vec<RemoteVmStatusOutput>>> {
    let mut host_vms_map = BTreeMap::new();
    for (hkey, host) in unique_hosts {
        let vms = fetch_host_vms(exec, &host.ssh, &host_bins[hkey], &host.vmm_home)?;
        host_vms_map.insert(hkey.clone(), vms);
    }
    Ok(host_vms_map)
}

fn find_observed_vm<'a>(
    host_vms_map: &'a BTreeMap<String, Vec<RemoteVmStatusOutput>>,
    hkey: &str,
    vm_name: &str,
) -> Option<&'a RemoteVmStatusOutput> {
    host_vms_map
        .get(hkey)
        .and_then(|vms| vms.iter().find(|vm| vm.name == vm_name))
}

fn require_observed_vm(
    host_vms_map: &BTreeMap<String, Vec<RemoteVmStatusOutput>>,
    hkey: &str,
    member: &DesiredMember,
) -> Result<RemoteVmStatusOutput> {
    find_observed_vm(host_vms_map, hkey, &member.vm_name)
        .cloned()
        .ok_or_else(|| {
            eyre::eyre!(
                "VM `{}` missing on host {} during rolling update",
                member.vm_name,
                member.vmm_host.host
            )
        })
}

#[allow(clippy::too_many_arguments)]
fn run_remote_vm_update(
    exec: &dyn HostExec,
    member: &DesiredMember,
    observed: &RemoteVmStatusOutput,
    bin: &RemoteOutbeUp,
    host_img: &RemoteImagePublishOutput,
    target_image_name: &str,
    target_image_hash: &str,
    stop_timeout: Duration,
) -> Result<RemoteVmUpdateOutput> {
    let update_cmd = bin.cmd(&format!(
        "vm update --name {} --expect-vm-id {} --image {} --image-name {} --stop-timeout {} --vmm-home {} --json",
        shell_quote(&member.vm_name),
        shell_quote(&observed.vm_id),
        shell_quote(&host_img.target_dir),
        shell_quote(target_image_name),
        stop_timeout.as_secs(),
        shell_quote_tilde_path(&member.vmm_home),
    ));
    let updated: RemoteVmUpdateOutput =
        run_host_json(exec, &member.vmm_host, &member.name, &update_cmd).wrap_err_with(|| {
            format!(
                "gracefully update VM `{}` ({}) on {} to `{target_image_name}`",
                member.vm_name, observed.vm_id, member.vmm_host.host
            )
        })?;
    ensure!(
        updated.vm_id == observed.vm_id
            && updated.image_name == target_image_name
            && updated.os_image_hash == target_image_hash,
        "unexpected `outbe-up vm update` output for `{}` on {}: got vm_id={} image={} ({}), expected vm_id={} image={target_image_name} ({target_image_hash})",
        member.vm_name,
        member.vmm_host.host,
        updated.vm_id,
        updated.image_name,
        updated.os_image_hash,
        observed.vm_id
    );
    Ok(updated)
}

fn record_member_target_image(
    member: &mut DesiredMember,
    state: &mut NetworkVmState,
    state_lock: &crate::network::vm_state::NetworkVmStateLock,
    artifacts: &Path,
    observed: &RemoteVmStatusOutput,
    target_image_name: &str,
    target_image_hash: &str,
) -> Result<()> {
    member.image_name = Some(target_image_name.to_owned());
    member.image_hash = Some(target_image_hash.to_owned());
    state.upsert_member(build_member_state(
        member,
        target_image_hash,
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

// ---------------------------------------------------------------------------
// Health & DKG predicates (F2 + F4)
// ---------------------------------------------------------------------------

fn validator_required_peers(members: &[DesiredMember]) -> usize {
    members
        .iter()
        .filter(|m| m.role != MemberRole::FullNode)
        .count()
        .saturating_sub(1)
}

fn common_validator_vrf(statuses: &[VmMemberStatus]) -> Option<u64> {
    common_validator_vrf_excluding(statuses, None)
}

fn common_validator_vrf_excluding(
    statuses: &[VmMemberStatus],
    exclude_idx: Option<usize>,
) -> Option<u64> {
    let validators: Vec<&VmMemberStatus> = statuses
        .iter()
        .enumerate()
        .filter(|(i, s)| Some(*i) != exclude_idx && s.role != MemberRole::FullNode)
        .map(|(_, s)| s)
        .collect();
    if validators.is_empty() {
        return Some(0);
    }
    let versions: BTreeSet<u64> = validators
        .iter()
        .filter_map(|s| s.vrf_material_version)
        .collect();
    let reported_count = validators
        .iter()
        .filter(|s| s.vrf_material_version.is_some())
        .count();
    if reported_count == validators.len() && versions.len() == 1 {
        versions.into_iter().next()
    } else {
        None
    }
}

pub(crate) fn all_validators_randomness_healthy(statuses: &[VmMemberStatus]) -> bool {
    statuses
        .iter()
        .filter(|s| s.role != MemberRole::FullNode)
        .all(|s| {
            s.consensus
                .as_ref()
                .and_then(|c| c.randomness_status.as_deref())
                .is_some_and(|st| st.eq_ignore_ascii_case("healthy"))
        })
}

/// Fail-closed pre-stop check requiring every validator to report consensus
/// `connectedPeers >= n - 1` (never accepting degraded `0 < connectedPeers < n - 1` or missing
/// `outbe_consensusStatus`). Note: `outbe_consensusStatus.connectedPeers` counts signers in the
/// last finalized certificate (`>= n - 1`, up to `n` when all validators sign before assembly).
pub(crate) fn check_validators_connected_peers(
    statuses: &[VmMemberStatus],
    required_peers: usize,
) -> Result<()> {
    let min_peers = required_peers as u64;
    for s in statuses.iter().filter(|s| s.role != MemberRole::FullNode) {
        let consensus = s.consensus.as_ref().ok_or_else(|| {
            eyre::eyre!(
                "refusing to stop member: validator `{}` is missing `outbe_consensusStatus` (fail-closed peer check)",
                s.name
            )
        })?;
        ensure!(
            consensus.connected_peers >= min_peers,
            "refusing to stop member: validator `{}` reports consensus connectedPeers={} (expected >= n-1={min_peers})",
            s.name,
            consensus.connected_peers
        );
    }
    Ok(())
}

fn ensure_all_classified_ours(
    build: &NetworkArtifacts,
    sample: &NetworkSample,
    max_lag: u64,
    context: &str,
) -> Result<()> {
    let bad: Vec<String> = sample
        .statuses
        .iter()
        .filter(|s| s.classification != VmClassKind::Ours)
        .map(|s| format!("{}: classification={:?}", s.name, s.classification))
        .collect();
    if !bad.is_empty() {
        bail!(
            "{context} failed: {}\n\n{}",
            bad.join("; "),
            render_vm_status(build, &sample.statuses, &sample.orphans, max_lag)
        );
    }
    Ok(())
}

fn ensure_all_members_healthy(
    build: &NetworkArtifacts,
    sample: &NetworkSample,
    required_peers: usize,
    max_lag: u64,
    context: &str,
) -> Result<()> {
    let mut problems = Vec::new();
    if !sample.orphans.is_empty() {
        problems.push(format!("{} orphan VM(s) present", sample.orphans.len()));
    }
    for (s, a) in sample.statuses.iter().zip(&sample.assessments) {
        if s.classification != VmClassKind::Ours {
            problems.push(format!("{}: classification={:?}", s.name, s.classification));
        } else if !member_node_ready(s, build.config.chain_id, required_peers) {
            let reason = if let Some(err) = s.error.as_deref() {
                err.to_owned()
            } else if s.role != MemberRole::FullNode {
                match s.consensus.as_ref() {
                    None => "missing `outbe_consensusStatus` (fail-closed peer check)".to_owned(),
                    Some(c) if c.connected_peers < required_peers as u64 => format!(
                        "consensus connectedPeers={} (expected >= n-1={required_peers})",
                        c.connected_peers
                    ),
                    Some(_) => "not ready (vmm/rpc/consensus/tee)".to_owned(),
                }
            } else {
                "not ready (vmm/rpc/consensus/tee)".to_owned()
            };
            problems.push(format!("{}: {reason}", s.name));
        } else if a.lagging {
            problems.push(format!("{}: {}", s.name, a.reasons.join(", ")));
        }
    }
    if sample.has_validators && sample.common_vrf.is_none() {
        problems.push("validators do not agree on a common vrf_material_version".to_owned());
    }
    if !problems.is_empty() {
        bail!(
            "{context} failed: {}\n\n{}",
            problems.join("; "),
            render_vm_status(build, &sample.statuses, &sample.orphans, max_lag)
        );
    }
    Ok(())
}

/// Compute the effective DKG guard in finalized blocks from `stop_timeout`, a conservative
/// boot + TEE re-admission budget, the measured block rate, and a fixed safety margin, floored by
/// `floor_blocks`.
pub(crate) fn compute_effective_dkg_guard_blocks(
    stop_timeout: Duration,
    floor_blocks: u64,
    blocks_per_sec: f64,
) -> u64 {
    let safe_bps = if blocks_per_sec.is_finite() && blocks_per_sec > 0.0 {
        blocks_per_sec.clamp(0.1, 20.0)
    } else {
        1.0
    };
    let downtime_secs = stop_timeout.as_secs_f64() + DKG_BOOT_AND_TEE_BUDGET_SECS;
    let time_based = (downtime_secs * safe_bps).ceil() as u64 + DKG_GUARD_MARGIN_BLOCKS;
    time_based.max(floor_blocks)
}

async fn measure_blocks_per_sec(
    ctx: &PollContext<'_>,
    initial_sample: &NetworkSample,
) -> Result<f64> {
    tokio::time::sleep(BLOCK_RATE_SAMPLE_INTERVAL).await;
    let second = sample_network(ctx).await?;
    let dt = second
        .sampled_at
        .saturating_duration_since(initial_sample.sampled_at)
        .as_secs_f64();
    let dh = second
        .max_finalized()
        .unwrap_or(0)
        .saturating_sub(initial_sample.max_finalized().unwrap_or(0));
    if dt > 0.0 && dh > 0 {
        Ok((dh as f64) / dt)
    } else {
        Ok(1.0)
    }
}

enum MissedVoteOutcome {
    Clean,
    NeedsDkgWait(Option<String>),
}

/// Pre-stop gate run immediately before EVERY VM stop (F2 + F4 + forward missed-vote gate):
/// - Phase 1: takes a fresh `NetworkSample`, refuses to stop if ANY member is unhealthy, lagging,
///   split on VRF, missing `outbe_consensusStatus`, has validator `connectedPeers < n - 1`, or an
///   orphan exists. For validators, fails closed if `next_planned_activation_height` is 0/missing
///   and defers if inside an active DKG ceremony or within `effective_dkg_guard + 60` blocks of
///   `next_planned_activation_height`.
/// - Phase 2: observes `MISSED_VOTE_OBSERVATION_BLOCKS` (60) finalized blocks across all active
///   validators via `IValidatorSet.getActiveValidators` + `validatorByAddress` and requires `missedVotes`
///   delta == 0 for every validator. If an epoch boundary falls inside OR any validator records a
///   non-zero missed-vote delta, resets the baseline, re-checks the DKG guard (returning to
///   Phase 1 if inside the guard/ceremony), and retries the 60-block window until `deadline`.
///   Fails closed on RPC/health/peer error or on `deadline` with the last offending deltas.
async fn pre_stop_health_and_dkg_gate(
    ctx: &PollContext<'_>,
    member: &DesiredMember,
    effective_dkg_guard: u64,
    deadline: Instant,
    progress: &mut Progress,
) -> Result<()> {
    let mut last_offenders: Option<String> = None;

    loop {
        let initial_sample = if member.role == MemberRole::FullNode {
            let sample = sample_network(ctx).await?;
            ensure_all_members_healthy(
                ctx.build,
                &sample,
                ctx.required_peers,
                ctx.max_lag,
                &format!("pre-stop health check before updating `{}`", member.name),
            )?;
            check_validators_connected_peers(&sample.statuses, ctx.required_peers)?;
            sample
        } else {
            let mut waited_boundary: Option<u64> = None;
            let outcome = poll_network_until(ctx, deadline, progress, |sample| {
                // F4: before entering a boundary wait, any unhealthy network state or degraded
                // validator consensus peer count (`connectedPeers < n - 1`) aborts immediately
                // rather than stopping another validator.
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
                let next_act = consensus.next_planned_activation_height;
                ensure!(
                    next_act > 0,
                    "refusing to stop validator `{}`: `next_planned_activation_height` is 0 or missing in `outbe_consensusStatus` (fail-closed DKG guard)",
                    member.name
                );
                let randomness_healthy = all_validators_randomness_healthy(&sample.statuses);

                if let Some(boundary) = waited_boundary {
                    if sample.fully_healthy()
                        && check_validators_connected_peers(&sample.statuses, ctx.required_peers)
                            .is_ok()
                        && randomness_healthy
                        && finalized > boundary
                        && next_act > boundary
                    {
                        return Ok(PollStep::Done(sample.clone_light()));
                    }
                    return Ok(PollStep::Continue(format!(
                        "waiting past DKG boundary {boundary} before updating `{}` (finalized={finalized}, next_activation={next_act}, randomness_healthy={randomness_healthy})",
                        member.name
                    )));
                }

                let remaining = next_act.saturating_sub(finalized);
                let in_ceremony = !randomness_healthy
                    || consensus
                        .last_dkg_activation_height
                        .is_some_and(|last| last >= next_act);
                let guard_with_obs =
                    effective_dkg_guard.saturating_add(MISSED_VOTE_OBSERVATION_BLOCKS);
                if in_ceremony || remaining <= guard_with_obs {
                    waited_boundary = Some(next_act);
                    return Ok(PollStep::Continue(format!(
                        "deferring stop of `{}` near/during DKG ceremony (finalized={finalized}, next_activation={next_act}, remaining={remaining}, effective_guard={effective_dkg_guard}, observation_window={MISSED_VOTE_OBSERVATION_BLOCKS}, randomness_healthy={randomness_healthy})",
                        member.name
                    )));
                }

                // Fresh sample just passed health/peer checks and is outside guard/ceremony.
                Ok(PollStep::Done(sample.clone_light()))
            })
            .await?;

            match outcome {
                Ok(sample) => sample,
                Err(sample) => {
                    let offenders_note = last_offenders
                        .as_deref()
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
        };

        if !initial_sample.has_validators {
            return Ok(());
        }

        // Phase 2: Forward 60-block missed-vote observation (`IValidatorSet.validatorByAddress`
        // `missedVotes` delta == 0 for every validator; retries window on epoch boundary or
        // transient missed votes up to `deadline`, re-checking DKG guard each retry).
        match observe_zero_missed_votes_window(
            ctx,
            member,
            &initial_sample,
            effective_dkg_guard,
            deadline,
            progress,
            &mut last_offenders,
        )
        .await?
        {
            MissedVoteOutcome::Clean => return Ok(()),
            MissedVoteOutcome::NeedsDkgWait(note) => {
                if let Some(msg) = note {
                    progress.update(&msg);
                }
                continue;
            }
        }
    }
}

/// Forward observation before stopping `member`: snapshot `IValidatorSet.validatorByAddress` for every
/// active validator, wait `MISSED_VOTE_OBSERVATION_BLOCKS` (60) finalized blocks, and snapshot
/// again. Requires `missedVotes` delta == 0 for every validator. Retries the 60-block window up
/// to `deadline` on epoch boundaries / counter resets or transient missed votes (re-checking the
/// DKG guard before each retry and returning `NeedsDkgWait` if inside the ceremony/guard window).
/// Fails closed on RPC/health/peer error or on `deadline` with the last offending deltas.
async fn observe_zero_missed_votes_window(
    ctx: &PollContext<'_>,
    member: &DesiredMember,
    initial_sample: &NetworkSample,
    effective_dkg_guard: u64,
    deadline: Instant,
    progress: &mut Progress,
    last_offenders: &mut Option<String>,
) -> Result<MissedVoteOutcome> {
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

    let mut start_height = initial_sample.min_finalized().ok_or_else(|| {
        eyre::eyre!(
            "refusing to stop `{}`: missing finalized height for missed-vote observation (fail-closed)",
            member.name
        )
    })?;
    let mut start_vrf = initial_sample.common_vrf.ok_or_else(|| {
        eyre::eyre!(
            "refusing to stop `{}`: missing common vrf_material_version for missed-vote observation (fail-closed)",
            member.name
        )
    })?;
    let mut start_missed = query_validators_missed_votes(
        ctx.exec.as_ref(),
        &probe.vmm_host,
        &probe.name,
        probe.service_ip,
        probe.rpc_port,
    )
    .wrap_err_with(|| {
        format!(
            "refusing to stop `{}`: failed initial `IValidatorSet.validatorByAddress` check (fail-closed)",
            member.name
        )
    })?;

    progress.update(&format!(
        "observing {} finalized blocks for 0 missed votes across {} validator(s) before updating `{}` (start={start_height}, vrf={start_vrf})",
        MISSED_VOTE_OBSERVATION_BLOCKS,
        start_missed.len(),
        member.name
    ));

    let guard_with_obs = effective_dkg_guard.saturating_add(MISSED_VOTE_OBSERVATION_BLOCKS);
    let outcome = poll_network_until(ctx, deadline, progress, |sample| {
        ensure_all_members_healthy(
            ctx.build,
            sample,
            ctx.required_peers,
            ctx.max_lag,
            &format!(
                "pre-stop missed-vote observation before updating `{}`",
                member.name
            ),
        )?;
        check_validators_connected_peers(&sample.statuses, ctx.required_peers)?;

        let cur_height = sample.min_finalized().ok_or_else(|| {
            eyre::eyre!(
                "refusing to stop `{}`: missing finalized height during missed-vote observation",
                member.name
            )
        })?;
        let cur_vrf = sample.common_vrf.ok_or_else(|| {
            eyre::eyre!(
                "refusing to stop `{}`: lost common vrf_material_version during missed-vote observation",
                member.name
            )
        })?;

        // Check whether a validator has drifted into an active DKG ceremony during observation.
        let validator_needs_dkg_wait = |check_guard_margin: bool| -> Result<bool> {
            if member.role == MemberRole::FullNode {
                return Ok(false);
            }
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
            let next_act = consensus.next_planned_activation_height;
            ensure!(
                next_act > 0,
                "refusing to stop validator `{}`: `next_planned_activation_height` is 0 or missing in `outbe_consensusStatus` (fail-closed DKG guard)",
                member.name
            );
            let randomness_healthy = all_validators_randomness_healthy(&sample.statuses);
            let in_ceremony = !randomness_healthy
                || consensus
                    .last_dkg_activation_height
                    .is_some_and(|last| last >= next_act);
            if in_ceremony {
                return Ok(true);
            }
            if check_guard_margin {
                let remaining = next_act.saturating_sub(cur_height);
                if remaining <= guard_with_obs {
                    return Ok(true);
                }
            }
            Ok(false)
        };

        // Epoch boundary fell inside the window -> re-check DKG guard and retry 60 blocks cleanly.
        if cur_vrf != start_vrf {
            if validator_needs_dkg_wait(true)? {
                return Ok(PollStep::Done(MissedVoteOutcome::NeedsDkgWait(None)));
            }
            start_height = cur_height;
            start_vrf = cur_vrf;
            start_missed = query_validators_missed_votes(
                ctx.exec.as_ref(),
                &probe.vmm_host,
                &probe.name,
                probe.service_ip,
                probe.rpc_port,
            )
            .wrap_err_with(|| {
                format!(
                    "refusing to stop `{}`: failed post-boundary `IValidatorSet.validatorByAddress` check (fail-closed)",
                    member.name
                )
            })?;
            return Ok(PollStep::Continue(format!(
                "epoch boundary crossed during missed-vote window before updating `{}`; retrying {} blocks from height {start_height} (vrf={start_vrf})",
                member.name, MISSED_VOTE_OBSERVATION_BLOCKS
            )));
        }

        let target_height = start_height.saturating_add(MISSED_VOTE_OBSERVATION_BLOCKS);
        if cur_height < target_height {
            return Ok(PollStep::Continue(format!(
                "observing missed votes before updating `{}`: finalized={cur_height}/{target_height} (vrf={cur_vrf})",
                member.name
            )));
        }

        let end_missed = query_validators_missed_votes(
            ctx.exec.as_ref(),
            &probe.vmm_host,
            &probe.name,
            probe.service_ip,
            probe.rpc_port,
        )
        .wrap_err_with(|| {
            format!(
                "refusing to stop `{}`: failed end-of-window `IValidatorSet.validatorByAddress` check (fail-closed)",
                member.name
            )
        })?;

        // Detect an epoch-boundary counter reset (`end < start`) or validator-set change at an
        // epoch boundary -> re-check DKG guard and retry the 60-block window cleanly.
        let counter_reset = start_missed.iter().any(|(addr, &before)| {
            end_missed
                .get(addr)
                .is_some_and(|&after| after < before)
        }) || end_missed.len() != start_missed.len()
            || !start_missed.keys().all(|k| end_missed.contains_key(k));
        if counter_reset {
            if validator_needs_dkg_wait(true)? {
                return Ok(PollStep::Done(MissedVoteOutcome::NeedsDkgWait(None)));
            }
            start_height = cur_height;
            start_vrf = cur_vrf;
            start_missed = end_missed;
            return Ok(PollStep::Continue(format!(
                "participation counters reset at epoch boundary before updating `{}`; retrying {} blocks from height {start_height}",
                member.name, MISSED_VOTE_OBSERVATION_BLOCKS
            )));
        }

        // Check missed-vote deltas across all validators. If any validator missed >= 1 vote,
        // record the offending deltas, re-check the DKG guard, reset the baseline, and retry
        // another 60-block window until `deadline`.
        let elapsed = cur_height.saturating_sub(start_height);
        let mut offenders = Vec::new();
        for (addr, &before) in &start_missed {
            let after = end_missed[addr];
            let delta = after.saturating_sub(before);
            if delta > 0 {
                offenders.push(format!("{addr}: +{delta} ({before} -> {after})"));
            }
        }
        if !offenders.is_empty() {
            let summary = format!(
                "{} over {elapsed} finalized blocks ({start_height}..={cur_height})",
                offenders.join(", ")
            );
            *last_offenders = Some(summary.clone());
            if validator_needs_dkg_wait(true)? {
                return Ok(PollStep::Done(MissedVoteOutcome::NeedsDkgWait(Some(
                    format!(
                        "missed votes observed before updating `{}` ({summary}); deferring past next DKG boundary before retrying",
                        member.name
                    ),
                ))));
            }
            start_height = cur_height;
            start_vrf = cur_vrf;
            start_missed = end_missed;
            return Ok(PollStep::Continue(format!(
                "missed votes observed before updating `{}` ({summary}); retrying {} blocks from height {start_height} (vrf={start_vrf})",
                member.name, MISSED_VOTE_OBSERVATION_BLOCKS
            )));
        }

        // Verify we did not drift into an active DKG ceremony while observing the clean window.
        if validator_needs_dkg_wait(false)? {
            return Ok(PollStep::Done(MissedVoteOutcome::NeedsDkgWait(None)));
        }

        Ok(PollStep::Done(MissedVoteOutcome::Clean))
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
                "refusing to stop `{}`: timed out observing {} clean finalized blocks with 0 missed votes (start_height={start_height}, start_vrf={start_vrf}{offenders_detail}):\n\n{}",
                member.name,
                MISSED_VOTE_OBSERVATION_BLOCKS,
                render_vm_status(ctx.build, &sample.statuses, &sample.orphans, ctx.max_lag)
            );
        }
    }
}

async fn wait_post_vm_update_lockstep(
    ctx: &PollContext<'_>,
    updated_idx: usize,
    updated_vm_id: &str,
    min_finalized_blocks: u64,
    deadline: Instant,
    progress: &mut Progress,
) -> Result<()> {
    let updated_member = &ctx.desired_members[updated_idx];
    let ref_idx = ctx
        .desired_members
        .iter()
        .enumerate()
        .find(|(i, m)| *i != updated_idx && m.role != MemberRole::FullNode)
        .map(|(i, _)| i)
        .unwrap_or(updated_idx);
    let ref_member = &ctx.desired_members[ref_idx];

    let mut baseline: Option<u64> = None;
    let mut consecutive_ready: u32 = 0;

    let outcome = poll_network_until(ctx, deadline, progress, |sample| {
        if sample.fully_healthy() && baseline.is_none() {
            baseline = compute_readiness_baseline(&sample.statuses);
        }

        let mut sample_ok = false;
        if sample.fully_healthy() {
            if let Some(b) = baseline {
                let target = b.saturating_add(min_finalized_blocks);
                let growth_ok = sample
                    .statuses
                    .iter()
                    .all(|s| s.finalized.is_some_and(|h| h >= target));
                if growth_ok {
                    let hash_ok = if ref_idx == updated_idx {
                        true
                    } else {
                        verify_finalized_hash_match(
                            ctx.exec.as_ref(),
                            updated_member,
                            &sample.statuses[updated_idx],
                            ref_member,
                            &sample.statuses[ref_idx],
                        )?
                    };
                    if hash_ok {
                        sample_ok = true;
                    }
                }
            }
        }

        if sample_ok {
            consecutive_ready += 1;
            if consecutive_ready >= READY_CONSECUTIVE_SAMPLES {
                return Ok(PollStep::Done(()));
            }
        } else {
            consecutive_ready = 0;
        }

        Ok(PollStep::Continue(format!(
            "post-update `{}`: min_finalized={:?} baseline={:?} target=+{min_finalized_blocks} ready_streak={consecutive_ready}/{READY_CONSECUTIVE_SAMPLES}",
            updated_member.name,
            sample.min_finalized(),
            baseline
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
                updated_vm_id,
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

async fn wait_post_rollout_dkg_boundary(
    ctx: &PollContext<'_>,
    deadline: Instant,
    progress: &mut Progress,
) -> Result<Vec<VmMemberStatus>> {
    let mut start_vrf: Option<u64> = None;
    let mut target_activation: Option<u64> = None;

    let outcome = poll_network_until(ctx, deadline, progress, |sample| {
        if start_vrf.is_none() {
            if let Some(v) = sample.common_vrf {
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
                start_vrf = Some(v);
                target_activation = Some(next_act);
            }
        }

        if let (Some(base_vrf), Some(cur_vrf), Some(target_act)) =
            (start_vrf, sample.common_vrf, target_activation)
        {
            let min_finalized = sample.min_finalized().unwrap_or(0);
            let randomness_healthy = all_validators_randomness_healthy(&sample.statuses);
            if sample.fully_healthy()
                && randomness_healthy
                && cur_vrf > base_vrf
                && min_finalized >= target_act
            {
                let mut all_hashes_match = true;
                for i in 1..ctx.desired_members.len() {
                    if !verify_finalized_hash_match(
                        ctx.exec.as_ref(),
                        &ctx.desired_members[0],
                        &sample.statuses[0],
                        &ctx.desired_members[i],
                        &sample.statuses[i],
                    )? {
                        all_hashes_match = false;
                        break;
                    }
                }
                if all_hashes_match {
                    return Ok(PollStep::Done((
                        sample.statuses.clone(),
                        base_vrf,
                        cur_vrf,
                        min_finalized,
                    )));
                }
            }
        }

        Ok(PollStep::Continue(format!(
            "waiting for DKG boundary: start_vrf={:?} cur_vrf={:?} min_finalized={:?} target_activation={:?}",
            start_vrf,
            sample.common_vrf,
            sample.min_finalized(),
            target_activation
        )))
    })
    .await?;

    let (statuses, base_vrf, cur_vrf, min_finalized) = match outcome {
        Ok(res) => res,
        Err(sample) => {
            bail!(
                "timed out waiting for post-update DKG boundary crossing (start_vrf={:?}, target_activation={:?}):\n\n{}",
                start_vrf,
                target_activation,
                render_vm_status(ctx.build, &sample.statuses, &sample.orphans, ctx.max_lag)
            );
        }
    };

    progress.update(&format!(
        "DKG boundary crossed in lockstep: vrf_material_version {base_vrf} -> {cur_vrf} at min_finalized={min_finalized}"
    ));
    Ok(statuses)
}
