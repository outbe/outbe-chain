//! Stopping a LocalNet owner and sweeping its run-scoped resources.

use std::process::{Child, Command};
use std::thread::sleep;
use std::time::{Duration, Instant};

use eyre::{ensure, Result, WrapErr};

use super::{
    ensure_matches, process_identity, read_bootstrap, read_state, remove_state, state_is_live,
    LocalnetCli, LocalnetStateV1, STOP_TIMEOUT,
};
use crate::internal::config::Config;
use crate::world::localnet::Localnet;

pub(super) fn stop(cli: &LocalnetCli) -> Result<()> {
    let Some(state) = read_state(&cli.data_dir)? else {
        println!("LocalNet is already stopped");
        return Ok(());
    };
    ensure_matches(cli, &state)?;
    if !state_is_live(&state) {
        cleanup_stale_owner(cli, &state)?;
        cleanup_run_scoped(cli)?;
        remove_state(&cli.data_dir)?;
        println!("Removed stale LocalNet resources and owner record");
        return Ok(());
    }
    signal_owner(state.owner_pid)?;
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !state_is_live(&state) {
            remove_state(&cli.data_dir)?;
            println!("LocalNet stopped");
            return Ok(());
        }
        sleep(Duration::from_millis(100));
    }
    force_stop_live_owner(cli, &state)?;
    remove_state(&cli.data_dir)?;
    println!(
        "LocalNet owner pid {} required forced process-group cleanup",
        state.owner_pid
    );
    Ok(())
}

pub(super) fn terminate_startup_owner(cli: &LocalnetCli, child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    signal_owner(child.id())?;
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        sleep(Duration::from_millis(100));
    }

    signal_process_group(child.id(), "KILL")?;
    let _ = child.wait();
    cleanup_run_scoped(cli).wrap_err("clean resources after forced LocalNet owner stop")
}

fn signal_owner(pid: u32) -> Result<()> {
    signal_pid(pid, "TERM")
}

fn signal_pid(pid: u32, signal: &str) -> Result<()> {
    let status = Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status()
        .wrap_err("signal LocalNet owner")?;
    ensure!(status.success(), "failed to signal LocalNet owner");
    Ok(())
}

fn signal_process_group(group: u32, signal: &str) -> Result<()> {
    let status = Command::new("kill")
        .args([format!("-{signal}"), "--".to_owned(), format!("-{group}")])
        .status()
        .wrap_err("signal LocalNet owner process group")?;
    ensure!(
        status.success(),
        "failed to signal LocalNet process group {group}"
    );
    Ok(())
}

pub(super) fn cleanup_stale_owner(_cli: &LocalnetCli, state: &LocalnetStateV1) -> Result<()> {
    for process in &state.ocomp_processes {
        if process_identity(process.pid).as_deref() == Some(&process.process_identity) {
            signal_pid(process.pid, "TERM")?;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline
        && state.ocomp_processes.iter().any(|process| {
            process_identity(process.pid).as_deref() == Some(&process.process_identity)
        })
    {
        sleep(Duration::from_millis(100));
    }
    for process in &state.ocomp_processes {
        if process_identity(process.pid).as_deref() == Some(&process.process_identity) {
            signal_pid(process.pid, "KILL")?;
        }
    }
    Ok(())
}

fn force_stop_live_owner(cli: &LocalnetCli, state: &LocalnetStateV1) -> Result<()> {
    ensure!(state_is_live(state), "refuse to kill a reused owner pid");
    signal_process_group(state.owner_pid, "KILL")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && state_is_live(state) {
        sleep(Duration::from_millis(100));
    }
    ensure!(
        !state_is_live(state),
        "LocalNet owner survived forced process-group cleanup"
    );
    cleanup_run_scoped(cli)
}

pub(super) fn cleanup_run_scoped(cli: &LocalnetCli) -> Result<()> {
    let receipt = read_bootstrap(cli)?;
    let env = cli.environment(Some(&receipt.port_blocks))?;
    let config = Config::resolve(&env);
    let mut localnet = Localnet::new(config);
    localnet.teardown()?;
    Ok(())
}
