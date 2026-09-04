//! A local EVM chain the committee can be bridged to.
//!
//! Cross-chain scenarios need a second chain to send to; in production that is
//! a remote venue, here it is a local `anvil`. The process is owned like every
//! other launched process, so a dropped `World` takes it down.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

use alloy_primitives::{Address, B256};
use alloy_sol_types::sol;
use eyre::{bail, eyre, Result};

use crate::internal::config::Config;
use crate::internal::eth;
use crate::internal::proc::{attach_log, wait_tcp, ChildGuard};
use crate::world::forge::{self, address_from, DEPLOYER_ADDRESS, DEPLOYER_KEY, SALT_VERSION};

/// Which role id to read off a venue contract.
enum Role {
    Relayer,
}

/// Chain id of the local target chain, distinct from the committee's so the two
/// are never confused by cross-chain addressing.
pub const TARGET_CHAIN_ID: u64 = 31338;

/// Seconds to wait for the chain to accept connections.
const READY_TRIES: u32 = 60;

/// Addresses one target-chain deploy produced.
/// Native float the router holds to pay dispatch fees, as the deploy tops it up
/// on a real target. A user pays for their own NFT hop, so the bridge needs none.
const BRIDGE_FLOAT_WEI: u64 = 1_000_000_000_000_000_000;

#[derive(Clone, Debug)]
pub struct TargetContracts {
    pub create3_factory: Address,
    pub mailbox: Address,
    pub bridge: Address,
    pub intex_nft: Address,
    pub escrow: Address,
    pub auction: Address,
    pub nft_bridge: Address,
    pub target_router: Address,
    pub payment_token: Address,
    pub compact: Address,
}

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IEscrowWire {
        function wire(address intexAuction, address compact, address paymentToken) external;
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface ITargetRouterWire {
        function wire(address auction, address intex, address escrowAdapter) external;
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IAuctionWire {
        function wire(address escrow) external;
    }
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IVenueRoles {
        function grantRole(bytes32 role, address account) external;
        function hasRole(bytes32 role, address account) external view returns (bool);
        function RELAYER_ROLE() external view returns (bytes32);
    }
}

#[derive(Debug)]
pub struct TargetChain {
    cfg: Config,
    port: Option<u16>,
    guard: Option<ChildGuard>,
}

impl TargetChain {
    /// Idle handle - scenarios that never ask for a target chain pay nothing.
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
            // The harness prices its sends for the committee chain, whose base fee is
            // a handful of units. This chain carries messages, not a fee market, so it
            // charges nothing rather than forcing a second price everywhere.
            "--base-fee".to_owned(),
            "0".to_owned(),
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
        let shared = self.cfg.repo.join("contracts/shared");
        let intex = self.cfg.repo.join("contracts/intex");
        let chain_id = TARGET_CHAIN_ID.to_string();

        let create3_factory = address_from(
            &forge::run(
                &shared,
                &[
                    "script",
                    "script/DeployCreate3Factory.s.sol:DeployCreate3Factory",
                ],
                &[("CONTRACT_SALT", SALT_VERSION.to_owned())],
                &url,
            )?,
            "CREATE3_FACTORY_ADDRESS=",
        )?;

        // A mailbox the adapter can hold. Delivery across two chains stays a relay
        // concern, and this mock is the half that makes one possible: it records a
        // dispatch and emits it rather than delivering inline.
        let mailbox = address_from(
            &forge::run_with_ctor(
                &crosschain,
                &["create", "test/mocks/MockRelayMailbox.sol:MockRelayMailbox"],
                &[&chain_id],
                &[],
                &url,
            )?,
            "Deployed to:",
        )?;

        let bridge = address_from(
            &forge::run(
                &crosschain,
                &["script", "script/DeployAll.s.sol:DeployAll"],
                &[
                    // The bridge has to know the way home before anything is
                    // addressed back to the committee.
                    ("REMOTE_CHAIN_IDS", origin_chain_id.to_string()),
                    ("CONTRACT_SALT", SALT_VERSION.to_owned()),
                    ("BRIDGE_OWNER", DEPLOYER_ADDRESS.to_owned()),
                    ("CREATE3_FACTORY_ADDRESS", format!("{create3_factory:?}")),
                    ("HYPERLANE_MAILBOX", format!("{mailbox:?}")),
                    ("ACTIVE_GATEWAY", "hyperlane".to_owned()),
                ],
                &url,
            )?,
            "ERC7786Bridge:",
        )?;

        let venue = forge::run(
            &intex,
            &["script", "deploy/DeployTarget.s.sol:DeployTarget"],
            &[
                ("BRIDGE_ADDRESS", format!("{bridge:?}")),
                ("ORIGIN_CHAIN_ID", origin_chain_id.to_string()),
                ("TARGET_CHAIN_IDS", chain_id.clone()),
                ("SALT_VERSION", SALT_VERSION.to_owned()),
            ("CREATE3_FACTORY_ADDRESS", format!("{create3_factory:?}")),
                // Without a proceeds route the escrow refuses to finalise a day,
                // exactly as it would on the committee-side venue.
                ("WCOEN_BRIDGE", DEPLOYER_ADDRESS.to_owned()),
            ],
            &url,
        )?;

        // Bids are bonded and paid here, so the venue needs a real ERC-20 and the
        // compact lock behind it, exactly as the committee-side venue does.
        let payment_token = address_from(
            &forge::run(
                &intex,
                &["create", "test/mocks/MockWCOEN.sol:MockWCOEN"],
                &[],
                &url,
            )?,
            "Deployed to:",
        )?;
        let compact = address_from(
            &forge::run(
                &intex,
                &["create", "test/mocks/MockTheCompact.sol:MockTheCompact"],
                &[],
                &url,
            )?,
            "Deployed to:",
        )?;

        Ok(TargetContracts {
            create3_factory,
            mailbox,
            bridge,
            intex_nft: address_from(&venue, "IntexNFT1155:")?,
            escrow: address_from(&venue, "EscrowAdapter:")?,
            auction: address_from(&venue, "IntexAuction:")?,
            nft_bridge: address_from(&venue, "IntexNFT1155Bridge:")?,
            target_router: address_from(&venue, "TargetRouter:")?,
            payment_token,
            compact,
        })
    }

    /// Point the freshly deployed contracts at each other and let the router act.
    ///
    /// The auction's windows are stamped in committee time, but this chain keeps
    /// its own wall clock and would sit in the commit stage forever. Carrying the
    /// committee's clock over is what a chain with a real block cadence gets for
    /// free.
    pub fn sync_clock_to(&self, timestamp: u64) -> Result<()> {
        let url = self
            .rpc_url()
            .ok_or_else(|| eyre!("syncing the clock needs a running target chain"))?;
        crate::internal::eth::raw_json_with_params(
            &url,
            "anvil_setTime",
            serde_json::json!([format!("0x{timestamp:x}")]),
        );
        crate::internal::eth::raw_json_with_params(&url, "evm_mine", serde_json::json!([]));
        Ok(())
    }

    /// The deploy scripts stop at standing contracts on purpose - wiring is a
    /// separate step in production too. Without it the router holds no references
    /// and may not mint, so an inbound message would arrive and do nothing.
    pub fn wire(&self, contracts: &TargetContracts) -> Result<()> {
        let url = self
            .rpc_url()
            .ok_or_else(|| eyre!("wiring needs a running target chain"))?;

        self.send(
            &url,
            contracts.target_router,
            &ITargetRouterWire::wireCall {
                auction: contracts.auction,
                intex: contracts.intex_nft,
                escrowAdapter: contracts.escrow,
            },
        )?;
        self.send(
            &url,
            contracts.auction,
            &IAuctionWire::wireCall {
                escrow: contracts.escrow,
            },
        )?;

        // The router is what an inbound message becomes, so it is the account
        // allowed to create series and mint. The bridge needs the same right on the
        // collection to burn a holder's units here and mint them at home.
        // A router pays the bridge fee out of its own native float and reverts
        // `NotEnoughNative` when it holds none; production tops it up, and so
        // must a chain that is expected to send anything home.
        crate::internal::eth::send_value(
            &url,
            contracts.target_router,
            DEPLOYER_KEY,
            alloy_primitives::U256::from(BRIDGE_FLOAT_WEI),
        )?;
        self.send(
            &url,
            contracts.escrow,
            &IEscrowWire::wireCall {
                intexAuction: contracts.auction,
                compact: contracts.compact,
                paymentToken: contracts.payment_token,
            },
        )?;
        let relayer = self.role(&url, contracts.intex_nft, Role::Relayer)?;
        for (holder, role, account) in [
            (contracts.intex_nft, relayer, contracts.target_router),
            (contracts.auction, relayer, contracts.target_router),
            (contracts.escrow, relayer, contracts.target_router),
            (contracts.intex_nft, relayer, contracts.nft_bridge),
        ] {
            self.send(&url, holder, &IVenueRoles::grantRoleCall { role, account })?;
        }
        Ok(())
    }

    fn send<C: alloy_sol_types::SolCall>(
        &self,
        url: &str,
        to: Address,
        call: &C,
    ) -> Result<String> {
        eth::send_call(url, to, DEPLOYER_KEY, call, None)
    }

    /// Whether `account` holds `role` on `holder`.
    pub fn holds_role(&self, holder: Address, role_id: B256, account: Address) -> Result<bool> {
        let url = self
            .rpc_url()
            .ok_or_else(|| eyre!("role check needs a running target chain"))?;
        eth::read_call(
            &url,
            holder,
            &IVenueRoles::hasRoleCall {
                role: role_id,
                account,
            },
        )
        .ok_or_else(|| eyre!("read hasRole from {holder}"))
    }

    /// The relayer role id as `holder` defines it.
    pub fn relayer_role(&self, holder: Address) -> Result<B256> {
        let url = self
            .rpc_url()
            .ok_or_else(|| eyre!("role read needs a running target chain"))?;
        self.role(&url, holder, Role::Relayer)
    }

    fn role(&self, url: &str, holder: Address, role: Role) -> Result<B256> {
        let read = match role {
            Role::Relayer => eth::read_call(url, holder, &IVenueRoles::RELAYER_ROLECall {}),
        };
        read.ok_or_else(|| eyre!("read the role id from {holder}"))
    }
}

/// An OS-assigned free port, so the committee's own contiguous port blocks stay
/// untouched.
fn free_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}
