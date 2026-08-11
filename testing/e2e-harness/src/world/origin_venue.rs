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

/// Addresses one origin-side deploy produced.
#[derive(Clone, Debug)]
pub struct OriginContracts {
    pub create_x: Address,
    pub mailbox: Address,
    pub bridge: Address,
    pub loopback: Address,
    pub origin_router: Address,
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

    let origin = forge::run(
        &intex,
        &["script", "deploy/DeployOrigin.s.sol:DeployOrigin"],
        &[
            (
                "BRIDGE_ADDRESS",
                format!("{:?}", address_from(&hub, "ERC7786Bridge:")?),
            ),
            ("TARGET_CHAIN_IDS", chain.clone()),
            ("SALT_VERSION", SALT_VERSION.to_owned()),
        ],
        url,
    )?;

    Ok(OriginContracts {
        create_x,
        mailbox,
        bridge: address_from(&hub, "ERC7786Bridge:")?,
        loopback: address_from(&hub, "LoopbackGatewayAdapter:")?,
        origin_router: address_from(&origin, "OriginRouter:")?,
    })
}
