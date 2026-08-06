//! A local EVM chain the committee can be bridged to.
//!
//! Cross-chain scenarios need a second chain to send to; in production that is
//! a remote venue, here it is a local `anvil`. The process is owned like every
//! other launched process, so a dropped `World` takes it down.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use alloy_primitives::Address;
use eyre::{bail, eyre, Result};

use crate::internal::config::Config;
use crate::internal::proc::{attach_log, wait_tcp, ChildGuard};

/// Chain id of the local target chain, distinct from the committee's so the two
/// are never confused by cross-chain addressing.
pub const TARGET_CHAIN_ID: u64 = 31338;

/// Seconds to wait for the chain to accept connections.
const READY_TRIES: u32 = 60;

/// The chain's first prefunded account. Its key is public by construction, which
/// is the point: a throwaway chain is deployed without a production key. The
/// `e2e-test` contract addresses the node is built against derive from it.
const DEPLOYER_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const DEPLOYER_ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

/// CREATE3 salt namespace for the throwaway set, kept away from the pinned one.
const SALT_VERSION: &str = "e2e-test";

/// Addresses one target-chain deploy produced.
#[derive(Clone, Debug)]
pub struct TargetContracts {
    pub create_x: Address,
    pub mailbox: Address,
    pub bridge: Address,
    pub intex_nft: Address,
    pub target_router: Address,
}

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

    /// Deploy the cross-chain hub and the intex venue onto the running chain.
    ///
    /// Runs the project's own deploy scripts rather than reimplementing them, so
    /// the chain ends up with what a real venue has. `origin_chain_id` is the
    /// committee's, which the target router is peered against.
    pub fn deploy(&self, origin_chain_id: u64) -> Result<TargetContracts> {
        let url = self
            .rpc_url()
            .ok_or_else(|| eyre!("deploy needs a running target chain"))?;
        let crosschain = self.cfg.repo.join("contracts/crosschain");
        let intex = self.cfg.repo.join("contracts/intex");
        let chain_id = TARGET_CHAIN_ID.to_string();

        let create_x = address_from(
            &self.forge(
                &crosschain,
                &["script", "script/0_DeployCreateX.s.sol:DeployCreateXDeterministic"],
                &[("CONTRACT_SALT", SALT_VERSION.to_owned())],
                &url,
            )?,
            "CreateX deployed at:",
        )?;

        // A mailbox the adapter can hold; delivery across two chains is a relay
        // concern, not a deploy one.
        let mailbox = address_from(
            &self.forge(
                &crosschain,
                &[
                    "create",
                    "test/mocks/MockHyperlaneMailbox.sol:MockHyperlaneMailbox",
                    "--constructor-args",
                    &chain_id,
                ],
                &[],
                &url,
            )?,
            "Deployed to:",
        )?;

        let bridge = address_from(
            &self.forge(
                &crosschain,
                &["script", "script/DeployAll.s.sol:DeployAll"],
                &[
                    ("CONTRACT_SALT", SALT_VERSION.to_owned()),
                    ("BRIDGE_OWNER", DEPLOYER_ADDRESS.to_owned()),
                    ("CREATEX_ADDRESS", format!("{create_x:?}")),
                    ("HYPERLANE_MAILBOX", format!("{mailbox:?}")),
                    ("ACTIVE_GATEWAY", "hyperlane".to_owned()),
                ],
                &url,
            )?,
            "ERC7786Bridge:",
        )?;

        let venue = self.forge(
            &intex,
            &["script", "deploy/DeployTarget.s.sol:DeployTarget"],
            &[
                ("BRIDGE_ADDRESS", format!("{bridge:?}")),
                ("ORIGIN_CHAIN_ID", origin_chain_id.to_string()),
                ("TARGET_CHAIN_IDS", chain_id.clone()),
                ("SALT_VERSION", SALT_VERSION.to_owned()),
            ],
            &url,
        )?;

        Ok(TargetContracts {
            create_x,
            mailbox,
            bridge,
            intex_nft: address_from(&venue, "IntexNFT1155:")?,
            target_router: address_from(&venue, "TargetRouter:")?,
        })
    }

    /// Run one forge command in `dir` against this chain, returning its output.
    ///
    /// The environment is inherited so a mise-provisioned foundry resolves; only
    /// the deploy inputs the scripts read are added.
    fn forge(&self, dir: &Path, args: &[&str], env: &[(&str, String)], url: &str) -> Result<String> {
        let mut cmd = Command::new("forge");
        cmd.current_dir(dir)
            .args(args)
            .args(["--broadcast", "--rpc-url", url]);
        // `forge create` takes the key directly; scripts read it from the env.
        if args.first() == Some(&"create") {
            cmd.args(["--private-key", DEPLOYER_KEY]);
        } else {
            cmd.env("DEPLOYER_PK", DEPLOYER_KEY)
                .env("DEPLOYER_PRIVATE_KEY", DEPLOYER_KEY);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output()?;
        if !out.status.success() {
            bail!(
                "forge {} in {} failed: {}",
                args.join(" "),
                dir.display(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// The address a deploy script logged after `label`.
fn address_from(output: &str, label: &str) -> Result<Address> {
    output
        .lines()
        .rev()
        .find_map(|line| line.split_once(label))
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .ok_or_else(|| eyre!("no address logged after {label:?}"))?
        .parse()
        .map_err(|error| eyre!("unparseable address after {label:?}: {error}"))
}

/// An OS-assigned free port, so the committee's own contiguous port blocks stay
/// untouched.
fn free_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}
