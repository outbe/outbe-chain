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
pub(crate) const DESIS: &str = "0x0000000000000000000000000000000000001016";
const INTEX_FACTORY: &str = "0x0000000000000000000000000000000000001015";
const SYSTEM_CALLER: &str = "0xff00000000000000000000000000000000000001";

const RELAY_FLOAT_WEI: u128 = 1_000_000_000_000_000_000;

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
    pub intex_auction: Address,
    pub escrow: Address,
    pub nft_bridge: Address,
    /// What bids are bonded and paid in, and the custody the escrow locks into.
    pub payment_token: Address,
    pub compact: Address,
    pub target_router: Address,
    /// Unwraps inbound proceeds to native; funded so it can pay.
    pub wcoen: Address,
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

    // Proceeds arrive as WCOEN and are unwrapped to native. The repository's own
    // WETH-style stub pays from its balance, so nothing new is written here; the
    // deployer stands in for the token bridge, which is only an authorised caller.
    let wcoen = address_from(
        &forge::run(
            &intex,
            &[
                "create",
                "test/foundry/cross-chain/OriginRouterProceeds.t.sol:MockWCOEN",
            ],
            &[],
            url,
        )?,
        "Deployed to:",
    )?;

    let origin = forge::run(
        &intex,
        &["script", "deploy/DeployOrigin.s.sol:DeployOrigin"],
        &[
            ("BRIDGE_ADDRESS", format!("{bridge:?}")),
            ("TARGET_CHAIN_IDS", chain.clone()),
            ("SALT_VERSION", SALT_VERSION.to_owned()),
            ("OUTBE_WCOEN_BRIDGE", DEPLOYER_ADDRESS.to_owned()),
            ("OUTBE_WCOEN_TOKEN", format!("{wcoen:?}")),
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

    // Bids are bonded and paid in wCOEN held under a compact lock. Both are the
    // repository's own mocks: the payment token has to be a real ERC-20 for the
    // escrow to move, which the proceeds stub above deliberately is not.
    let payment_token = address_from(
        &forge::run(
            &intex,
            &["create", "test/mocks/MockWCOEN.sol:MockWCOEN"],
            &[],
            url,
        )?,
        "Deployed to:",
    )?;
    let compact = address_from(
        &forge::run(
            &intex,
            &["create", "test/mocks/MockTheCompact.sol:MockTheCompact"],
            &[],
            url,
        )?,
        "Deployed to:",
    )?;

    let contracts = OriginContracts {
        create_x,
        mailbox,
        bridge,
        loopback: address_from(&hub, "LoopbackGatewayAdapter:")?,
        origin_router: address_from(&origin, "OriginRouter:")?,
        intex_nft: address_from(&venue, "IntexNFT1155:")?,
        intex_auction: address_from(&venue, "IntexAuction:")?,
        escrow: address_from(&venue, "EscrowAdapter:")?,
        nft_bridge: address_from(&venue, "IntexNFT1155Bridge:")?,
        payment_token,
        compact,
        target_router: address_from(&venue, "TargetRouter:")?,
        wcoen,
    };
    wire(&intex, &contracts, url, chain_id)?;

    // Both routers pay the bridge fee from their own float when a chain-native
    // module triggers the send.
    for router in [contracts.origin_router, contracts.target_router] {
        crate::internal::eth::send_value(
            url,
            router,
            forge::DEPLOYER_KEY,
            alloy_primitives::U256::from(RELAY_FLOAT_WEI),
        )?;
    }
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
            ("--token", nft.clone()),
            ("--adapter", INTEX_FACTORY.to_owned()),
            ("--contract", "IntexNFT1155".to_owned()),
        ],
        url,
        chain_id,
    )?;

    let auction = format!("{:?}", contracts.intex_auction);
    let escrow = format!("{:?}", contracts.escrow);

    // The venue half: an inbound stage start is delivered by the target router,
    // which can only reach the auction once it holds its address.
    hardhat::task(
        intex,
        "target-bridge-wire",
        &[
            (
                "--bridge-contract",
                format!("{:?}", contracts.target_router),
            ),
            ("--intex-auction-contract", auction.clone()),
            ("--intex-contract", nft.clone()),
            ("--escrow-contract", escrow.clone()),
            (
                "--nft-bridge-contract",
                format!("{:?}", contracts.nft_bridge),
            ),
        ],
        url,
        chain_id,
    )?;

    hardhat::task(
        intex,
        "auction-wire",
        &[
            ("--intex-auction-contract", auction.clone()),
            ("--escrow-contract", escrow.clone()),
        ],
        url,
        chain_id,
    )?;

    hardhat::task(
        intex,
        "escrow-wire",
        &[
            ("--escrow-contract", escrow.clone()),
            ("--intex-auction-contract", auction.clone()),
            ("--compact-contract", format!("{:?}", contracts.compact)),
            ("--payment-token", format!("{:?}", contracts.payment_token)),
        ],
        url,
        chain_id,
    )?;

    // The venue accepts an inbound stage start, escrow move or mint only from a
    // relayer, and the target router is it.
    let target_router = format!("{:?}", contracts.target_router);
    let nft_bridge = format!("{:?}", contracts.nft_bridge);
    for (token, adapter, contract) in [
        (auction, target_router.clone(), "IntexAuction"),
        (escrow, target_router.clone(), "EscrowAdapter"),
        (nft.clone(), target_router, "IntexNFT1155"),
        (nft.clone(), nft_bridge.clone(), "IntexNFT1155"),
    ] {
        hardhat::task(
            intex,
            "grant-relayer-role",
            &[
                ("--token", token),
                ("--adapter", adapter),
                ("--contract", contract.to_owned()),
            ],
            url,
            chain_id,
        )?;
    }

    // A loopback target keeps its winners on the shared collection and never
    // drives migration, so it skips the bridge-side wiring a remote target needs.
    hardhat::task(
        intex,
        "grant-system-relayer-role",
        &[
            ("--token", nft),
            ("--adapter", nft_bridge),
            ("--contract", "IntexNFT1155".to_owned()),
        ],
        url,
        chain_id,
    )
}
