//! Native localnet process owner and shutdown supervision.
use super::{
    ensure_not_running, observe_heights, process_identity, read_bootstrap, rpc_ports, state_path,
    write_json_atomic, DriverPhase, LocalnetCli, LocalnetStateV1, StateGuard, START_TIMEOUT,
    STATE_VERSION,
};
use crate::internal::{config::Config, eth};
use crate::world::localnet::{Localnet, StartOpts};
use eyre::{bail, ensure, eyre, Result};
use std::time::{Duration, Instant};

pub(super) async fn serve(cli: &LocalnetCli) -> Result<()> {
    serve_native(cli).await
}

async fn serve_native(cli: &LocalnetCli) -> Result<()> {
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
    let binding =
        outbe_evm::tee_attestation_activation::DcapSeededChainSpecBindingV1::from_genesis_path(
            &config.dir.join("genesis.json"),
        )
        .map_err(|error| eyre!("derive native genesis identity: {error}"))?;
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
    };
    write_json_atomic(&state_path(&cli.data_dir), &state)?;
    let _state_guard = StateGuard(state_path(&cli.data_dir));

    localnet.start(&StartOpts::default())?;
    state.heights =
        wait_for_advancing_heights(&mut localnet, &state.rpc_ports, START_TIMEOUT).await?;
    verify_rpc_chain_identity(&state.rpc_ports, binding.chain_id, binding.genesis_hash)?;
    state.phase = DriverPhase::Running;
    write_json_atomic(&state_path(&cli.data_dir), &state)?;

    monitor_until_shutdown(&mut localnet).await?;
    localnet.stop()?;
    Ok(())
}

fn verify_rpc_chain_identity(
    ports: &[u16],
    chain_id_expected: u64,
    genesis_hash: alloy_primitives::B256,
) -> Result<()> {
    let expected_genesis = format!("{:#x}", genesis_hash);
    for port in ports {
        let url = format!("http://127.0.0.1:{port}");
        let chain_id = eth::raw_json(&url, "eth_chainId")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| eyre!("RPC {url} did not report a valid chain ID"))?;
        ensure!(
            chain_id == chain_id_expected,
            "RPC {url} chain ID {chain_id} differs from native genesis identity {}",
            chain_id_expected
        );
        let genesis = eth::block_hash(&url, 0)
            .ok_or_else(|| eyre!("RPC {url} did not return genesis block 0"))?;
        ensure!(
            genesis.eq_ignore_ascii_case(&expected_genesis),
            "RPC {url} genesis {genesis} differs from native genesis identity {expected_genesis}"
        );
    }
    Ok(())
}

async fn monitor_until_shutdown(localnet: &mut Localnet) -> Result<()> {
    let shutdown = wait_for_shutdown_signal();
    tokio::pin!(shutdown);
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            () = &mut shutdown => return Ok(()),
            _ = interval.tick() => {
                localnet.ensure_committee_alive()?;
            }
        }
    }
}

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
