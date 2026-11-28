//! Joining-validator VM orchestration (`outbe-net-up network add-validator`).

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
    time::Instant,
};

use alloy_primitives::{Address, U256};
use eyre::{bail, ensure, Result};
use outbe_operator_node_role::{
    progress::Progress, rpc::ConsensusStatus, validator_address, FEEDER_FUNDING_WEI,
};
use outbe_operator_remote::HostExec;

use super::super::{
    assess_members_lag, collect_unique_hosts, compute_readiness_baseline, member_node_ready,
    query_members_and_orphans, render_vm_status, DesiredMember, MemberLagAssessment, RemoteOutbeUp,
    UniqueHost, VmClassKind, VmMemberStatus, READY_CONSECUTIVE_SAMPLES,
};
use super::{
    driver::JoinChainDriver,
    full_node::{fetch_member_serial_tail, verify_finalized_hash_match},
    keys::ensure_local_member_keys,
    load_and_verify_member_preflight,
    provision::{
        provision_member, start_provisioned_vm, MemberDiskSource, ProvisionRequest,
        ProvisionedMember,
    },
    resolve_member_host_bins, sleep_until_poll_tick, AddValidatorOptions,
};
use crate::network::{
    config::FounderPublishedPorts, remote::NetworkArtifacts, vm_state::MemberRole,
};

const ORACLE_DELEGATE_ROLE: u8 = 1;
/// Explicit gas reserve (2 COEN) funded onto a joining validator alongside `min_stake` and
/// `FEEDER_FUNDING_WEI` BEFORE `vm start`, covering paid `registerEnclave` (not ZeroFee),
/// `staking stake`, `validator delegate oracle`, feeder funding, and `validator confirm-ready`.
const JOINER_GAS_RESERVE_WEI: u128 = 2_000_000_000_000_000_000;

struct JoinerPollCtx<'a> {
    build: &'a NetworkArtifacts,
    member: &'a DesiredMember,
    unique_hosts: &'a BTreeMap<String, UniqueHost>,
    host_bins: &'a BTreeMap<String, RemoteOutbeUp>,
    exec: &'a Arc<dyn HostExec>,
    driver: &'a dyn JoinChainDriver,
    max_lag: u64,
    deadline: Instant,
}

struct ActivationWaitCtx<'a> {
    options: &'a AddValidatorOptions<'a>,
    build: &'a NetworkArtifacts,
    member: &'a DesiredMember,
    active_validators: &'a [DesiredMember],
    unique_val_hosts: &'a BTreeMap<String, UniqueHost>,
    val_host_bins: &'a BTreeMap<String, RemoteOutbeUp>,
    target_bin: &'a RemoteOutbeUp,
    vm_id: &'a str,
    validator_addr: Address,
    exec: &'a Arc<dyn HostExec>,
    driver: &'a dyn JoinChainDriver,
    deadline: Instant,
}

pub(crate) async fn add_validator_with_exec(
    options: &AddValidatorOptions<'_>,
    exec: Arc<dyn HostExec>,
    driver: &dyn JoinChainDriver,
) -> Result<()> {
    let deadline = Instant::now() + options.timeout;
    let mut progress = Progress::new();

    // Generate or reuse joining validator keys locally before provisioning the disk.
    // Fails closed if the member keys directory is partially populated (F5).
    let build_probe =
        load_and_verify_member_preflight(options.artifacts, exec.as_ref(), &mut progress).await?;
    let ports = FounderPublishedPorts::from_offset(options.port_offset)?;
    progress.stage(&format!(
        "ensuring local joining-validator keys for `{}`",
        options.name
    ));
    let local_keys_dir =
        ensure_local_member_keys(&build_probe.root, options.name, build_probe.config.chain_id)?;
    let validator_addr = validator_address(&local_keys_dir.join("evm-key.hex"))?;
    let feeder_addr = validator_address(&local_keys_dir.join("feeder-key.hex"))?;

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
            role: MemberRole::JoiningValidator,
            name: options.name,
            host: options.host,
            user: options.user,
            port: options.port,
            vmm_home: options.vmm_home,
            bridge: options.bridge,
            vm_ip: options.vm_ip,
            rpc_bind_ip: options.rpc_bind_ip,
            advertise_ip: options.advertise_ip,
            rpc_port: options.rpc_port,
            port_offset: options.port_offset,
            image_source: options.image_source.as_ref(),
            vcpu: options.vcpu,
            memory_gib: options.memory_gib,
            disk_gib: options.disk_gib,
            no_tee: options.no_tee,
        },
        &MemberDiskSource::JoiningValidator {
            local_keys_dir: &local_keys_dir,
            validator_addr,
            ports,
        },
        exec.as_ref(),
        &mut progress,
    )?;

    let min_stake = ensure_joiner_prefunded(
        &build,
        &member,
        validator_addr,
        driver,
        (deadline, &mut progress),
    )
    .await?;

    start_provisioned_vm(exec.as_ref(), &target_bin, &member, &vm_id, &mut progress)?;

    let active_validators: Vec<DesiredMember> = all_members
        .iter()
        .filter(|m| m.role != MemberRole::FullNode)
        .cloned()
        .collect();
    let unique_val_hosts = collect_unique_hosts(&active_validators);
    let val_host_bins = resolve_member_host_bins(
        exec.as_ref(),
        &build.root,
        &outbe_up_sha256,
        &unique_val_hosts,
        (target_hkey, target_bin.clone()),
    )?;

    let poll_ctx = JoinerPollCtx {
        build: &build,
        member: &member,
        unique_hosts: &unique_val_hosts,
        host_bins: &val_host_bins,
        exec: &exec,
        driver,
        max_lag: options.max_lag,
        deadline,
    };

    progress.stage(&format!(
        "waiting for `{}` RPC, resident TEE, and initial sync",
        member.name
    ));
    wait_for_joiner_synced(&poll_ctx, &mut progress).await?;

    bond_and_configure_joiner(
        &member,
        &local_keys_dir,
        (validator_addr, feeder_addr, min_stake, ports),
        driver,
        (deadline, &mut progress),
    )
    .await?;

    progress.stage(&format!(
        "waiting for `{}` to reach verifier mode before confirm-ready",
        member.name
    ));
    wait_for_joiner_verifier(&poll_ctx, &mut progress).await?;

    confirm_joiner_ready_if_pending(
        &member,
        &local_keys_dir.join("evm-key.hex"),
        validator_addr,
        driver,
        (deadline, &mut progress),
    )
    .await?;

    let act_ctx = ActivationWaitCtx {
        options,
        build: &build,
        member: &member,
        active_validators: &active_validators,
        unique_val_hosts: &unique_val_hosts,
        val_host_bins: &val_host_bins,
        target_bin: &target_bin,
        vm_id: &vm_id,
        validator_addr,
        exec: &exec,
        driver,
        deadline,
    };
    wait_for_joiner_activation(&act_ctx, &mut progress).await
}

async fn ensure_joiner_prefunded(
    build: &NetworkArtifacts,
    member: &DesiredMember,
    validator_addr: Address,
    driver: &dyn JoinChainDriver,
    (deadline, progress): (Instant, &mut Progress),
) -> Result<U256> {
    // F1 + F7: Fund the joining validator BEFORE `vm start` with `min_stake + FEEDER_FUNDING_WEI +
    // JOINER_GAS_RESERVE_WEI`. `registerEnclave` is a paid transaction (not ZeroFee), so the node
    // cannot obtain its resident TEE offer key (`offer_key_ready`) while its balance is zero.
    let founder0_key = build
        .root
        .join("secrets/founder-keys/validator-0/evm-key.hex");
    let min_stake = driver.min_stake().await?;
    let record = driver.validator(validator_addr).await?;
    if record.registered {
        return Ok(min_stake);
    }
    let hyperlane_funding = build
        .config
        .resolve_joining_hyperlane_config()?
        .signer_funding_wei
        .parse::<U256>()
        .map_err(|error| eyre::eyre!("invalid Hyperlane signer funding amount: {error}"))?;
    let required_balance = min_stake
        .checked_add(U256::from(FEEDER_FUNDING_WEI))
        .and_then(|v| v.checked_add(hyperlane_funding))
        .and_then(|v| v.checked_add(U256::from(JOINER_GAS_RESERVE_WEI)))
        .ok_or_else(|| eyre::eyre!("required joiner balance overflow"))?;
    let current_balance = driver.balance(validator_addr).await?;
    if current_balance >= required_balance {
        return Ok(min_stake);
    }
    ensure!(
        founder0_key.is_file(),
        "founder-0 key missing at {} (cannot fund joiner {validator_addr:#x})",
        founder0_key.display()
    );
    let topup = required_balance - current_balance;
    progress.stage(&format!(
        "funding joining validator `{}` ({validator_addr:#x}) before VM start with {topup} wei (min_stake + feeder + Hyperlane + gas reserve)",
        member.name
    ));
    let tx = driver
        .fund_address(&founder0_key, validator_addr, topup)
        .await?;
    driver.wait_tx_finalized(&tx, deadline).await?;
    ensure!(
        driver.balance(validator_addr).await? >= required_balance,
        "joining validator {validator_addr:#x} funding not visible after finality"
    );
    Ok(min_stake)
}

async fn bond_and_configure_joiner(
    member: &DesiredMember,
    local_keys_dir: &Path,
    (validator_addr, feeder_addr, min_stake, ports): (
        Address,
        Address,
        U256,
        FounderPublishedPorts,
    ),
    driver: &dyn JoinChainDriver,
    (deadline, progress): (Instant, &mut Progress),
) -> Result<()> {
    let mut record = driver.validator(validator_addr).await?;
    if !record.registered {
        let symmetric = format!("{}:{}", member.advertise_ip, ports.consensus_p2p_tcp);
        progress.stage(&format!(
            "bonding minimum stake ({min_stake} wei) for `{}` ({validator_addr:#x}, p2p {symmetric})",
            member.name
        ));
        let tx = driver.stake(local_keys_dir, validator_addr, min_stake, &symmetric)?;
        driver.wait_tx_finalized(&tx, deadline).await?;
        record = driver.validator(validator_addr).await?;
        ensure!(
            record.registered && matches!(record.status, 1 | 2),
            "joining validator {validator_addr:#x} not PENDING/ACTIVE after stake finality (registered={}, status={})",
            record.registered,
            record.status
        );
    }

    let joiner_evm_key = local_keys_dir.join("evm-key.hex");
    if driver
        .validator_delegate(validator_addr, ORACLE_DELEGATE_ROLE)
        .await?
        != feeder_addr
    {
        progress.stage(&format!(
            "delegating oracle role for `{}` to feeder {feeder_addr:#x}",
            member.name
        ));
        let tx = driver.delegate_oracle(&joiner_evm_key, feeder_addr)?;
        driver.wait_tx_finalized(&tx, deadline).await?;
        ensure!(
            driver
                .validator_delegate(validator_addr, ORACLE_DELEGATE_ROLE)
                .await?
                == feeder_addr,
            "oracle feeder delegation not visible after finality"
        );
    }

    if driver.balance(feeder_addr).await?.is_zero() {
        progress.stage(&format!(
            "funding oracle feeder {feeder_addr:#x} for `{}`",
            member.name
        ));
        let tx = driver
            .fund_address(&joiner_evm_key, feeder_addr, U256::from(FEEDER_FUNDING_WEI))
            .await?;
        driver.wait_tx_finalized(&tx, deadline).await?;
        ensure!(
            !driver.balance(feeder_addr).await?.is_zero(),
            "oracle feeder funding not visible after finality"
        );
    }
    Ok(())
}

async fn confirm_joiner_ready_if_pending(
    member: &DesiredMember,
    joiner_evm_key: &Path,
    validator_addr: Address,
    driver: &dyn JoinChainDriver,
    (deadline, progress): (Instant, &mut Progress),
) -> Result<()> {
    let record = driver.validator(validator_addr).await?;
    if record.status == 1 {
        progress.stage(&format!(
            "sending validator confirm-ready for `{}` ({validator_addr:#x})",
            member.name
        ));
        let tx = driver.confirm_ready(joiner_evm_key)?;
        driver.wait_tx_finalized(&tx, deadline).await?;
    }
    Ok(())
}

async fn wait_for_joiner_activation(
    ctx: &ActivationWaitCtx<'_>,
    progress: &mut Progress,
) -> Result<()> {
    progress.stage(&format!(
        "waiting for DKG reshare activation and +{} post-activation finalized blocks for `{}`",
        ctx.options.min_finalized_blocks, ctx.member.name
    ));
    let required_peers = ctx.active_validators.len().saturating_sub(1);
    let founder0 = ctx
        .active_validators
        .first()
        .ok_or_else(|| eyre::eyre!("no founding validator found"))?;
    let mut activation_baseline: Option<u64> = None;
    let mut consecutive_ready: u32 = 0;

    loop {
        let onchain = ctx.driver.validator(ctx.validator_addr).await?;
        let (statuses, _) = query_members_and_orphans(
            ctx.build,
            ctx.active_validators,
            ctx.unique_val_hosts,
            ctx.val_host_bins,
            ctx.exec,
        )
        .await?;
        let assessments = assess_members_lag(&statuses, ctx.options.max_lag);
        let joiner_status = statuses.iter().find(|s| s.vm_name == ctx.member.vm_name);
        let founder0_status = statuses.iter().find(|s| s.vm_name == founder0.vm_name);

        let activated = onchain.registered
            && onchain.status == 2
            && onchain.has_bls_share
            && joiner_status.is_some_and(|s| {
                s.classification == VmClassKind::Ours
                    && member_node_ready(s, ctx.build.config.chain_id, required_peers)
            });
        if activated && activation_baseline.is_none() {
            activation_baseline = compute_readiness_baseline(&statuses);
        }

        let sample_ready = activated
            && joiner_lockstep_sample_ready(
                ctx,
                (&statuses, &assessments),
                (founder0, joiner_status, founder0_status),
                (required_peers, activation_baseline),
            )?;

        consecutive_ready = if sample_ready {
            consecutive_ready + 1
        } else {
            0
        };
        if consecutive_ready >= READY_CONSECUTIVE_SAMPLES {
            progress.done(&format!(
                "joining validator `{}` ({:#x}) is ACTIVE signer in lockstep (baseline B={}, growth >= +{})",
                ctx.member.name,
                ctx.validator_addr,
                activation_baseline.unwrap_or(0),
                ctx.options.min_finalized_blocks,
            ));
            println!(
                "\n{}",
                render_vm_status(ctx.build, &statuses, &[], ctx.options.max_lag)
            );
            return Ok(());
        }

        if Instant::now() >= ctx.deadline {
            let tail =
                fetch_member_serial_tail(ctx.exec.as_ref(), ctx.member, ctx.vm_id, ctx.target_bin);
            bail!(
                "timed out after {}s waiting for joining validator `{}` ({:#x}) activation & lockstep (onchain status={}, has_bls_share={}, baseline={:?}):\n\n{}{}",
                ctx.options.timeout.as_secs(),
                ctx.member.name,
                ctx.validator_addr,
                onchain.status,
                onchain.has_bls_share,
                activation_baseline,
                render_vm_status(ctx.build, &statuses, &[], ctx.options.max_lag),
                tail,
            );
        }

        progress.update(&format!(
            "onchain_status={} bls_share={} joiner_mode={} baseline={:?} ready_streak={consecutive_ready}/{READY_CONSECUTIVE_SAMPLES}",
            onchain.status,
            onchain.has_bls_share,
            joiner_status
                .and_then(|s| s.consensus_mode.as_deref())
                .unwrap_or("-"),
            activation_baseline,
        ));
        sleep_until_poll_tick(ctx.deadline).await;
    }
}

fn joiner_lockstep_sample_ready(
    ctx: &ActivationWaitCtx<'_>,
    (statuses, assessments): (&[VmMemberStatus], &[MemberLagAssessment]),
    (founder0, joiner_status, founder0_status): (
        &DesiredMember,
        Option<&VmMemberStatus>,
        Option<&VmMemberStatus>,
    ),
    (required_peers, activation_baseline): (usize, Option<u64>),
) -> Result<bool> {
    let Some(b) = activation_baseline else {
        return Ok(false);
    };
    let (Some(js), Some(f0)) = (joiner_status, founder0_status) else {
        return Ok(false);
    };
    let target = b.saturating_add(ctx.options.min_finalized_blocks);
    let all_validators_ok = statuses.iter().zip(assessments).all(|(s, a)| {
        s.classification == VmClassKind::Ours
            && member_node_ready(s, ctx.build.config.chain_id, required_peers)
            && !a.lagging
            && s.finalized.is_some_and(|h| h >= target)
    });
    let vrf_versions: BTreeSet<u64> = statuses
        .iter()
        .filter_map(|s| s.vrf_material_version)
        .collect();
    if !all_validators_ok || vrf_versions.len() != 1 {
        return Ok(false);
    }
    verify_finalized_hash_match(ctx.exec.as_ref(), ctx.member, js, founder0, f0)
}

fn joiner_tee_ready(status: &VmMemberStatus) -> bool {
    status.no_tee
        || status
            .consensus
            .as_ref()
            .is_some_and(ConsensusStatus::resident_ready)
}

fn joiner_synced_to_tip(status: &VmMemberStatus, tip: Option<u64>, max_lag: u64) -> bool {
    status
        .finalized
        .zip(tip)
        .is_some_and(|(mine, remote)| remote.saturating_sub(mine) <= max_lag)
}

fn joiner_base_healthy(status: &VmMemberStatus, tip: Option<u64>, max_lag: u64) -> bool {
    let running_ok = status.vmm_running && status.error.is_none();
    let tee_ok = joiner_tee_ready(status);
    let feeder_ok = status.feeder_healthy == Some(true);
    let synced = joiner_synced_to_tip(status, tip, max_lag);
    running_ok && tee_ok && feeder_ok && synced
}

async fn wait_for_joiner_synced(ctx: &JoinerPollCtx<'_>, progress: &mut Progress) -> Result<()> {
    let target_slice = vec![ctx.member.clone()];
    loop {
        let tip = ctx.driver.finalized_block().await.ok().map(|b| b.number);
        let (statuses, _) = query_members_and_orphans(
            ctx.build,
            &target_slice,
            ctx.unique_hosts,
            ctx.host_bins,
            ctx.exec,
        )
        .await?;
        if let Some(s) = statuses.first() {
            let chain_ok = s.chain_id == Some(ctx.build.config.chain_id);
            if chain_ok && joiner_base_healthy(s, tip, ctx.max_lag) {
                return Ok(());
            }
            progress.update(&format!(
                "joiner `{}` boot/sync: finalized={:?} tip={:?} mode={:?} tee_ok={}",
                ctx.member.name,
                s.finalized,
                tip,
                s.consensus_mode,
                joiner_tee_ready(s),
            ));
        }
        ensure!(
            Instant::now() < ctx.deadline,
            "timed out waiting for joining validator `{}` RPC, resident TEE, and initial sync",
            ctx.member.name
        );
        sleep_until_poll_tick(ctx.deadline).await;
    }
}

async fn wait_for_joiner_verifier(ctx: &JoinerPollCtx<'_>, progress: &mut Progress) -> Result<()> {
    let target_slice = vec![ctx.member.clone()];
    loop {
        let tip = ctx.driver.finalized_block().await.ok().map(|b| b.number);
        let (statuses, _) = query_members_and_orphans(
            ctx.build,
            &target_slice,
            ctx.unique_hosts,
            ctx.host_bins,
            ctx.exec,
        )
        .await?;
        if let Some(s) = statuses.first() {
            let mode_ok = matches!(s.consensus_mode.as_deref(), Some("verifier" | "signer"));
            if mode_ok && joiner_base_healthy(s, tip, ctx.max_lag) {
                return Ok(());
            }
            progress.update(&format!(
                "joiner `{}` waiting for verifier: mode={:?} finalized={:?} tip={:?} tee_ok={}",
                ctx.member.name,
                s.consensus_mode,
                s.finalized,
                tip,
                joiner_tee_ready(s),
            ));
        }
        ensure!(
            Instant::now() < ctx.deadline,
            "timed out waiting for joining validator `{}` to enter verifier mode",
            ctx.member.name
        );
        sleep_until_poll_tick(ctx.deadline).await;
    }
}
