//! A local EVM chain the committee can be bridged to.
//!
//! Cross-chain scenarios need a second chain to send to; in production that is
//! a remote venue, here it is a local `anvil`. The process is owned like every
//! other launched process, so a dropped `World` takes it down.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

use eyre::{bail, Result};

use crate::internal::config::Config;
use crate::internal::proc::{attach_log, wait_tcp, ChildGuard};

/// Chain id of the local target chain, distinct from the committee's so the two
/// are never confused by cross-chain addressing.
pub const TARGET_CHAIN_ID: u64 = 31338;

/// Seconds to wait for the chain to accept connections.
const READY_TRIES: u32 = 60;

#[derive(Debug)]
pub struct TargetChain {
    cfg: Config,
    port: Option<u16>,
    guard: Option<ChildGuard>,
}

impl TargetChain {
    /// Idle handle — scenarios that never ask for a target chain pay nothing.
    pub(crate) fn new(cfg: Config) -> Self {
        Self {
            cfg,
            port: None,
            guard: None,
        }
    }

    /// Launch the chain on an OS-assigned free port.
    pub fn start(&mut self) -> Result<()> {
        if self.guard.is_some() {
            bail!("target chain is already running");
        }
        let port = free_port()?;
        let dir = self.dir();
        std::fs::create_dir_all(&dir)?;

        let mut cmd = Command::new("anvil");
        // The environment is inherited rather than replaced with `Config::path`:
        // that only appends `~/.foundry/bin`, while foundry is just as often
        // provisioned through mise somewhere else entirely.
        cmd.args([
            "--port".to_owned(),
            port.to_string(),
            "--chain-id".to_owned(),
            TARGET_CHAIN_ID.to_string(),
        ]);
        attach_log(&mut cmd, &dir)?;

        let guard = ChildGuard::spawn("anvil", cmd)?;
        if !wait_tcp(port, READY_TRIES) {
            bail!("target chain never accepted connections on port {port}");
        }
        self.port = Some(port);
        self.guard = Some(guard);
        Ok(())
    }

    pub fn port(&self) -> Option<u16> {
        self.port
    }

    pub fn rpc_url(&self) -> Option<String> {
        self.port.map(|port| format!("http://127.0.0.1:{port}"))
    }

    pub fn chain_id(&self) -> u64 {
        TARGET_CHAIN_ID
    }

    fn dir(&self) -> PathBuf {
        self.cfg.dir.join("target-chain")
    }
}

/// An OS-assigned free port, so the committee's own contiguous port blocks stay
/// untouched.
fn free_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}
