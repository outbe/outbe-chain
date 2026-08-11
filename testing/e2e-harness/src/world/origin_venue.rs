//! The intex engine deployed onto the committee's own chain.
//!
//! An auction has to reach a target chain, and the node can be its own: the
//! bridge routes the local chain through a loopback adapter, so the whole
//! issuance-and-proceeds path runs without a second process. A remote target
//! is the same deploy pointed at another endpoint.

use std::path::{Path, PathBuf};

use alloy_primitives::Address;
use eyre::Result;

use crate::world::forge::{self, address_from, DEPLOYER_ADDRESS, SALT_VERSION};
use crate::world::hardhat;

/// Outbe EVM precompiles the origin engine is wired to, and the frame
/// precompile-initiated calls arrive as.
const DESIS: &str = "0x0000000000000000000000000000000000001016";
const INTEX_FACTORY: &str = "0x0000000000000000000000000000000000001015";
const SYSTEM_CALLER: &str = "0xff00000000000000000000000000000000000001";

/// Addresses one origin-side deploy produced.
#[derive(Clone, Debug)]
pub struct OriginContracts {
    pub create_x: Address,
    pub mailbox: Address,
    pub bridge: Address,
    pub loopback: Address,
    pub origin_router: Address,
    /// The venue half: the committee is its own auction target, so the
    /// collection and router a remote chain would host sit here too.
    pub intex_nft: Address,
    pub target_router: Address,
}

/// Deploy the cross-chain hub and the origin engine onto the committee's chain.
///
/// Runs the project's own scripts so the chain ends up with what a real origin
/// has. `chain_id` is the committee's, registered as its own auction target:
/// that is what makes the loopback route reachable.
pub fn deploy(repo: &Path, url: &str, chain_id: u64) -> Result<OriginContracts> {
    let crosschain: PathBuf = repo.join("contracts/crosschain");
    let intex: PathBuf = repo.join("contracts/intex");
    let chain = chain_id.to_string();

    let create_x = address_from(
        &forge::run(
            &crosschain,
            &[
                "script",
                "script/0_DeployCreateX.s.sol:DeployCreateXDeterministic",
            ],
            &[("CONTRACT_SALT", SALT_VERSION.to_owned())],
            url,
        )?,
        "CreateX deployed at:",
    )?;

    // The hyperlane adapter needs a mailbox to hold even when nothing remote is
    // wired yet; the loopback route never touches it.
    let mailbox = address_from(
        &forge::run_with_ctor(
            &crosschain,
            &[
                "create",
                "test/mocks/MockHyperlaneMailbox.sol:MockHyperlaneMailbox",
            ],
            &[&chain],
            &[],
            url,
        )?,
        "Deployed to:",
    )?;

    let hub = forge::run(
        &crosschain,
        &["script", "script/DeployAll.s.sol:DeployAll"],
        &[
            ("CONTRACT_SALT", SALT_VERSION.to_owned()),
            ("BRIDGE_OWNER", DEPLOYER_ADDRESS.to_owned()),
            ("CREATEX_ADDRESS", format!("{create_x:?}")),
            ("HYPERLANE_MAILBOX", format!("{mailbox:?}")),
            ("ACTIVE_GATEWAY", "hyperlane".to_owned()),
            ("WIRE_LOOPBACK", "true".to_owned()),
        ],
        url,
    )?;

    let bridge = address_from(&hub, "ERC7786Bridge:")?;
    let origin = forge::run(
        &intex,
        &["script", "deploy/DeployOrigin.s.sol:DeployOrigin"],
        &[
            ("BRIDGE_ADDRESS", format!("{bridge:?}")),
            ("TARGET_CHAIN_IDS", chain.clone()),
            ("SALT_VERSION", SALT_VERSION.to_owned()),
        ],
        url,
    )?;

    // The committee is its own target, so the venue half lands on the same chain.
    let venue = forge::run(
        &intex,
        &["script", "deploy/DeployTarget.s.sol:DeployTarget"],
        &[
            ("BRIDGE_ADDRESS", format!("{bridge:?}")),
            ("ORIGIN_CHAIN_ID", chain.clone()),
            ("TARGET_CHAIN_IDS", chain.clone()),
            ("SALT_VERSION", SALT_VERSION.to_owned()),
        ],
        url,
    )?;

    let contracts = OriginContracts {
        create_x,
        mailbox,
        bridge,
        loopback: address_from(&hub, "LoopbackGatewayAdapter:")?,
        origin_router: address_from(&origin, "OriginRouter:")?,
        intex_nft: address_from(&venue, "IntexNFT1155:")?,
        target_router: address_from(&venue, "TargetRouter:")?,
    };
    wire(&intex, &contracts, url, chain_id)?;
    Ok(contracts)
}

/// Grant the roles a deploy script cannot: Desis and IntexFactory are
/// precompiles, and their calls arrive as the begin-block system frame rather
/// than from the precompile address. Production wires through these same tasks.
fn wire(intex: &Path, contracts: &OriginContracts, url: &str, chain_id: u64) -> Result<()> {
    let router = format!("{:?}", contracts.origin_router);
    let nft = format!("{:?}", contracts.intex_nft);

    hardhat::task(
        intex,
        "origin-bridge-wire",
        &[
            ("--bridge-contract", router.clone()),
            ("--desis-contract", DESIS.to_owned()),
            ("--intex-factory-contract", INTEX_FACTORY.to_owned()),
        ],
        url,
        chain_id,
    )?;

    hardhat::task(
        intex,
        "outbe-system-grant-roles",
        &[
            ("--bridge-contract", router),
            ("--intex-contract", nft.clone()),
            ("--system-address", SYSTEM_CALLER.to_owned()),
            ("--desis-contract", DESIS.to_owned()),
        ],
        url,
        chain_id,
    )?;

    hardhat::task(
        intex,
        "grant-relayer-role",
        &[
            ("--token", nft),
            ("--adapter", INTEX_FACTORY.to_owned()),
            ("--contract", "IntexNFT1155".to_owned()),
        ],
        url,
        chain_id,
    )
}
