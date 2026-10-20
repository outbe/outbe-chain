//! The currency an Intex settles in: a reference stablecoin and the reserve
//! vault that makes it acceptable.
//!
//! `IntexFactory.settle` refuses a token the VaultRouter holds no vault for, and
//! reads the token's ISO code to decide whether it answers the series' reference
//! or issuance currency. The auction's wCOEN satisfies neither, so settlement
//! needs its own asset.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use alloy_sol_types::sol;
use eyre::{ensure, eyre, Result};

use crate::internal::{addresses, eth};
use crate::world::forge::{self, address_from};
use crate::world::{validators::Operator, World};

/// ISO 4217 numeric for USD, the reference currency every e2e series uses.
pub(crate) const USD_ISO: u16 = 840;

const REGISTRATION_TIMEOUT_SECS: u64 = 60;
const APPROVAL_TIMEOUT_SECS: u64 = 180;

sol! {
    interface ISettlementAsset {
        function decimals() external view returns (uint8);
        function isoCode() external view returns (uint16);
    }

    interface ISettlementVault {
        function owner() external view returns (address);
        function asset() external view returns (address);
    }
}

/// The stablecoin owners settle in, and the vault their payment lands in.
#[derive(Clone, Copy, Debug)]
pub struct SettlementCurrency {
    pub asset: Address,
    pub vault: Address,
}

/// Deploy the asset and its vault, then register the vault with the VaultRouter.
///
/// The vault renounces its own owner in its constructor, then ACTIVE validators
/// approve its registration before the permissionless apply transaction.
pub(crate) fn deploy(
    world: &World,
    repo_intex: &std::path::Path,
    url: &str,
) -> Result<SettlementCurrency> {
    deploy_for(world, repo_intex, url, USD_ISO)
}

/// [`deploy`] for a stablecoin answering `iso_code` instead of USD.
pub(crate) fn deploy_for(
    world: &World,
    repo_intex: &std::path::Path,
    url: &str,
    iso_code: u16,
) -> Result<SettlementCurrency> {
    let asset = address_from(
        &forge::run_with_ctor(
            repo_intex,
            &[
                "create",
                "test/mocks/MockReferenceStablecoin.sol:MockReferenceStablecoin",
            ],
            &[&iso_code.to_string()],
            &[],
            url,
        )?,
        "Deployed to:",
    )?;

    let vault = address_from(
        &forge::run_with_ctor(
            repo_intex,
            &[
                "create",
                "test/mocks/MockSettlementVault.sol:MockSettlementVault",
            ],
            &[&format!("{asset:#x}")],
            &[],
            url,
        )?,
        "Deployed to:",
    )?;

    if eth::read_call(url, vault, &ISettlementVault::assetCall {}) != Some(asset) {
        return Err(eyre!("settlement vault does not hold the settlement asset"));
    }
    if eth::read_call(url, vault, &ISettlementVault::ownerCall {}) != Some(Address::ZERO) {
        return Err(eyre!("settlement vault has not renounced its owner"));
    }

    register_vault(world, asset, vault, iso_code)?;
    Ok(SettlementCurrency { asset, vault })
}

/// Register a deployed vault through the same vote-and-apply boundary as users.
pub(crate) fn register_vault(
    world: &World,
    asset: Address,
    vault: Address,
    iso_code: u16,
) -> Result<()> {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let active = world
        .rpc
        .active_validators(port)
        .ok_or_else(|| eyre!("read ACTIVE validators for vault registration"))?;
    let mut voters = Vec::new();
    for index in world.localnet.owned_validator_indices() {
        let validator = world.validators.get(index);
        let key = validator.evm_key()?;
        let address = world
            .rpc
            .address_of(&key)
            .ok_or_else(|| eyre!("read validator-{index} address"))?
            .parse::<Address>()?;
        if active.contains(&address) {
            voters.push(validator);
        }
    }
    let proposer = voters
        .first()
        .ok_or_else(|| eyre!("no discovered ACTIVE validator can propose vault registration"))?;
    let target = format!("{:#x}", addresses::VAULT_ROUTER_ADDR);
    let payload = serde_json::json!({
        "operation": "addVault",
        "vault": format!("{vault:#x}"),
        "asset": format!("{asset:#x}"),
        "isoCode": iso_code,
    })
    .to_string();
    let tx = world
        .rpc
        .send_propose(&Operator(proposer.clone()), &target, &payload)?;
    ensure!(
        world.rpc.wait_successful_receipt(&tx, 60),
        "vault registration proposal transaction failed: {tx}"
    );
    let proposal_id = world.rpc.proposal_id_from_receipt(port, &tx)?;
    let mut ballots = Vec::new();
    for voter in &voters {
        ballots.push(world.rpc.cast_vote(voter, proposal_id, true)?);
    }
    for ballot in ballots {
        ensure!(
            world.rpc.wait_successful_receipt(&ballot, 60),
            "vault registration proposal #{proposal_id} ballot failed: {ballot}"
        );
    }
    // Poll the staged action directly: block/CLI polling can consume much of its
    // short apply window. Keep the slower status query for timeout diagnostics.
    let deadline = Instant::now() + Duration::from_secs(APPROVAL_TIMEOUT_SECS);
    loop {
        if eth::read_call(
            &url,
            addresses::VAULT_ROUTER_ADDR,
            &eth::IVaultRouter::approvedVaultActionCall {
                proposalId: U256::from(proposal_id),
            },
        )
        .is_some_and(|record| {
            matches!(record.action, eth::IVaultRouter::VaultAction::AddVault)
                && record.vault == vault
        }) {
            break;
        }
        if Instant::now() >= deadline {
            let proposal = world.rpc.vote_status_on(port, proposal_id)?;
            return Err(eyre!(
                "vault proposal #{proposal_id} approval timed out (status: {})",
                proposal.status
            ));
        }
        sleep(Duration::from_millis(250));
    }
    let applied = eth::send_call(
        &url,
        addresses::VAULT_ROUTER_ADDR,
        &proposer.evm_key()?,
        &eth::IVaultRouter::applyApprovedCall {
            proposalId: U256::from(proposal_id),
        },
        None,
    )?;
    ensure!(
        world.rpc.wait_successful_receipt(&applied, 60),
        "apply vault proposal #{proposal_id} transaction failed: {applied}"
    );

    // An expired apply succeeds without registration, so verify the exact vault.
    let deadline = Instant::now() + Duration::from_secs(REGISTRATION_TIMEOUT_SECS);
    loop {
        if registered_vaults(&url, asset).contains(&vault) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "vault proposal #{proposal_id} applied but vault {vault:#x} is not registered (possibly expired)"
            ));
        }
        sleep(Duration::from_millis(500));
    }
}

/// The vaults the router routes `asset` to - the read `settle` gates on.
pub(crate) fn registered_vaults(url: &str, asset: Address) -> Vec<Address> {
    let count = eth::read_call(
        url,
        addresses::VAULT_ROUTER_ADDR,
        &eth::IVaultRouter::assetVaultsCountCall { asset },
    )
    .unwrap_or_default();
    (0..count.to::<u64>())
        .filter_map(|index| {
            eth::read_call(
                url,
                addresses::VAULT_ROUTER_ADDR,
                &eth::IVaultRouter::assetVaultAtCall {
                    asset,
                    index: U256::from(index),
                },
            )
        })
        .collect()
}

/// The asset's ISO 4217 code, which decides whether it answers a series' currency.
pub(crate) fn iso_code(url: &str, asset: Address) -> Option<u16> {
    eth::read_call(url, asset, &ISettlementAsset::isoCodeCall {})
}

/// What the reserve vault holds of the settlement asset at one exact block.
pub(crate) fn vault_balance_at(
    url: &str,
    asset: Address,
    vault: Address,
    height: u64,
) -> Option<U256> {
    eth::read_call_at(
        url,
        asset,
        &ISettlementAssetBalance::balanceOfCall { account: vault },
        height,
    )
}

sol! {
    interface ISettlementAssetBalance {
        function balanceOf(address account) external view returns (uint256);
    }
}
