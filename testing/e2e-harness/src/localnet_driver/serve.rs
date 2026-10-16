//! The foreground LocalNet owner: start, publish state, and supervise until shutdown.

#[cfg(feature = "ocomp-integration")]
use std::time::{Duration, Instant};

use eyre::Result;
#[cfg(feature = "ocomp-integration")]
use eyre::{bail, ensure, eyre};

#[cfg(feature = "ocomp-integration")]
use super::{
    ensure_not_running, ensure_ocomp_process_inventory, observe_heights, process_identity,
    read_bootstrap, rpc_ports, state_path, write_json_atomic, DriverPhase, LocalnetStateV1,
    OwnedOcompProcessV1, StateGuard, START_TIMEOUT, STATE_VERSION,
};
use super::{require_ocomp_integration_build, LocalnetCli};
#[cfg(feature = "ocomp-integration")]
use crate::internal::{config::Config, eth};
#[cfg(feature = "ocomp-integration")]
use crate::world::localnet::{Localnet, StartOpts};
#[cfg(feature = "ocomp-integration")]
use crate::world::ocomp::{
    OcompLaunchIdentityV1, OcompProcessRole, OcompRuntimeCountsV1, OcompTopology,
};

pub(super) async fn serve(cli: &LocalnetCli) -> Result<()> {
    require_ocomp_integration_build()?;
    serve_with_ocomp(cli).await
}

#[cfg(feature = "ocomp-integration")]
async fn serve_with_ocomp(cli: &LocalnetCli) -> Result<()> {
    let receipt = read_bootstrap(cli)?;
    ensure_not_running(&cli.data_dir)?;
    let env = cli.environment(Some(&receipt.port_blocks))?;
    env.ports.ensure_available(cli.validators)?;
    let config = Config::resolve(&env);
    let rpc_ports = rpc_ports(&config);
    ensure!(
        rpc_ports == receipt.rpc_ports,
        "serve RPC layout differs from bootstrap"
    );
    let mut localnet = Localnet::new(config.clone());
    let mut ocomp = OcompTopology::new(config);
    let ocomp_identity = ocomp.prepare_bootstrapped_runtime()?;
    let owner_pid = std::process::id();
    let owner_process_identity = process_identity(owner_pid)
        .ok_or_else(|| eyre!("cannot establish LocalNet owner process identity"))?;
    let mut state = LocalnetStateV1 {
        version: STATE_VERSION,
        lane: "dev_mock".to_owned(),
        phase: DriverPhase::Starting,
        owner_pid,
        owner_process_identity,
        repo: cli.repo.clone(),
        data_dir: cli.data_dir.clone(),
        validators: cli.validators,
        port_blocks: receipt.port_blocks,
        rpc_ports,
        heights: vec![0; cli.validators],
        ocomp_supervisors: 0,
        ocomp_snapshot_exporters: 0,
        ocomp_workers: 0,
        ocomp_registered_workers: 0,
        ocomp_connected_workers: 0,
        ocomp_processes: Vec::new(),
    };
    write_json_atomic(&state_path(&cli.data_dir), &state)?;
    let _state_guard = StateGuard(state_path(&cli.data_dir));

    localnet.start(&StartOpts::default())?;
    state.heights =
        wait_for_advancing_heights(&mut localnet, &state.rpc_ports, START_TIMEOUT).await?;
    verify_rpc_chain_identity(&state.rpc_ports, ocomp_identity)?;
    ocomp.start_baseline_runtime(ocomp_identity)?;
    let counts = wait_for_ocomp_runtime(&mut ocomp, START_TIMEOUT).await?;
    apply_ocomp_counts(&mut state, counts);
    state.ocomp_processes = snapshot_ocomp_processes(&ocomp, counts)?;
    state.phase = DriverPhase::Running;
    write_json_atomic(&state_path(&cli.data_dir), &state)?;

    monitor_until_shutdown(&mut localnet, &mut ocomp).await?;
    drop(ocomp);
    localnet.stop()?;
    Ok(())
}

#[cfg(not(feature = "ocomp-integration"))]
async fn serve_with_ocomp(_cli: &LocalnetCli) -> Result<()> {
    unreachable!("require_ocomp_integration_build rejects this build")
}

#[cfg(feature = "ocomp-integration")]
fn apply_ocomp_counts(state: &mut LocalnetStateV1, counts: OcompRuntimeCountsV1) {
    state.ocomp_supervisors = counts.supervisors;
    state.ocomp_snapshot_exporters = counts.snapshot_exporters;
    state.ocomp_workers = counts.workers;
    state.ocomp_registered_workers = counts.registered_workers;
    state.ocomp_connected_workers = counts.connected_workers;
}

#[cfg(feature = "ocomp-integration")]
fn snapshot_ocomp_processes(
    ocomp: &OcompTopology,
    counts: OcompRuntimeCountsV1,
) -> Result<Vec<OwnedOcompProcessV1>> {
    let mut processes = Vec::new();
    for record in ocomp
        .process_records()
        .iter()
        .filter(|record| record.stopped_at_millis.is_none())
    {
        let role = match record.role {
            OcompProcessRole::SnapshotExporter => "snapshot_exporter",
            OcompProcessRole::Worker => "worker",
        };
        let process_identity = process_identity(record.pid).ok_or_else(|| {
            eyre!(
                "OCOMP {role} pid {} exited before its exact identity was recorded",
                record.pid
            )
        })?;
        processes.push(OwnedOcompProcessV1 {
            validator_index: record.validator_index,
            role: role.to_owned(),
            worker_ordinal: record.worker_ordinal,
            pid: record.pid,
            process_identity,
        });
    }
    ensure_ocomp_process_inventory(&processes, counts)?;
    Ok(processes)
}

#[cfg(feature = "ocomp-integration")]
fn verify_rpc_chain_identity(ports: &[u16], identity: OcompLaunchIdentityV1) -> Result<()> {
    let expected_genesis = format!("{:#x}", identity.genesis_hash);
    for port in ports {
        let url = format!("http://127.0.0.1:{port}");
        let chain_id = eth::raw_json(&url, "eth_chainId")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| eyre!("RPC {url} did not report a valid chain ID"))?;
        ensure!(
            chain_id == identity.chain_id,
            "RPC {url} chain ID {chain_id} differs from OCOMP identity {}",
            identity.chain_id
        );
        let genesis = eth::block_hash(&url, 0)
            .ok_or_else(|| eyre!("RPC {url} did not return genesis block 0"))?;
        ensure!(
            genesis.eq_ignore_ascii_case(&expected_genesis),
            "RPC {url} genesis {genesis} differs from OCOMP identity {expected_genesis}"
        );
    }
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
async fn wait_for_ocomp_runtime(
    ocomp: &mut OcompTopology,
    timeout: Duration,
) -> Result<OcompRuntimeCountsV1> {
    let deadline = Instant::now() + timeout;
    loop {
        ocomp.ensure_baseline_processes_alive(1)?;
        match ocomp.observe_baseline_runtime(1) {
            Ok(counts) => return Ok(counts),
            Err(error) if Instant::now() >= deadline => {
                bail!(
                    "OCOMP Supervisors/Workers did not become ready within {} seconds: {error}",
                    timeout.as_secs()
                );
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(feature = "ocomp-integration")]
async fn monitor_until_shutdown(localnet: &mut Localnet, ocomp: &mut OcompTopology) -> Result<()> {
    let shutdown = wait_for_shutdown_signal();
    tokio::pin!(shutdown);
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            () = &mut shutdown => return Ok(()),
            _ = interval.tick() => {
                localnet.ensure_committee_alive()?;
                ocomp.ensure_baseline_runtime_ready(1)?;
            }
        }
    }
}

#[cfg(feature = "ocomp-integration")]
async fn wait_for_advancing_heights(
    localnet: &mut Localnet,
    ports: &[u16],
    timeout: Duration,
) -> Result<Vec<u64>> {
    let deadline = Instant::now() + timeout;
    let mut baseline: Option<Vec<u64>> = None;
    loop {
        localnet.ensure_committee_alive()?;
        if let Some(heights) = observe_heights(ports) {
            if heights.iter().all(|height| *height > 0) {
                match &baseline {
                    Some(first)
                        if heights
                            .iter()
                            .zip(first)
                            .all(|(height, first)| height > first) =>
                    {
                        return Ok(heights);
                    }
                    None => baseline = Some(heights),
                    Some(_) => {}
                }
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "all validator RPCs did not advance beyond genesis within {} seconds",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(feature = "ocomp-integration")]
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
