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
use outbe_vaultrouter::VaultRouterVoteAction;

use crate::internal::{addresses, eth};
use crate::world::forge::{self, address_from};
use crate::world::validators::Validator;
use crate::world::World;

/// ISO 4217 numeric for USD, the reference currency every e2e series uses.
pub(crate) const USD_ISO: u16 = 840;

const REGISTRATION_TIMEOUT_SECS: u64 = 60;
const RECEIPT_TRIES: u32 = 40;
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

/// Deploy the asset and its vault, then register the vault with the VaultRouter
/// by validator vote (see [`register_vault`]). The vault renounces its own owner
/// in its constructor, which the router demands before it will adopt one.
pub(crate) fn deploy(world: &World, repo_intex: &std::path::Path) -> Result<SettlementCurrency> {
    deploy_for(world, repo_intex, USD_ISO)
}

/// [`deploy`] for a stablecoin answering `iso_code` instead of USD.
pub(crate) fn deploy_for(
    world: &World,
    repo_intex: &std::path::Path,
    iso_code: u16,
) -> Result<SettlementCurrency> {
    let url = world.rpc.url(world.validators.primary_port());
    let url = url.as_str();
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

    register_vault(world, vault, asset, iso_code)?;

    // Confirm the registration itself rather than its receipt. `settle` reads this
    // count, and a zero here is the failure the whole fixture exists to avoid.
    let deadline = Instant::now() + Duration::from_secs(REGISTRATION_TIMEOUT_SECS);
    loop {
        let registered = eth::read_call(
            url,
            addresses::VAULT_ROUTER_ADDR,
            &eth::IVaultRouter::assetVaultsCountCall { asset },
        )
        .is_some_and(|count| !count.is_zero());
        if registered {
            return Ok(SettlementCurrency { asset, vault });
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "VaultRouter still holds no vault for the settlement asset"
            ));
        }
        sleep(Duration::from_millis(500));
    }
}

/// Register `vault` with the VaultRouter through the production path. The router
/// has no owner. The first ACTIVE validator proposes AddVault, every ACTIVE
/// validator approves, and the tally records the approved action. Then the
/// proposer applies it with `applyApproved`.
///
/// The localnet voting window is a few blocks wide, and an approved action
/// expires after the same number of blocks. So the ballots go out before any
/// receipt wait, and the apply goes out as soon as the approval is visible.
pub(crate) fn register_vault(
    world: &World,
    vault: Address,
    asset: Address,
    iso_code: u16,
) -> Result<()> {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let voters = active_validators(world)?;
    let proposer = voters
        .first()
        .ok_or_else(|| eyre!("no ACTIVE validator key can propose AddVault"))?
        .evm_key()?;

    let payload = VaultRouterVoteAction::AddVault {
        vault,
        asset,
        iso_code,
    }
    .to_json();
    let proposal = eth::send_call(
        &url,
        addresses::VOTE_ADDR,
        &proposer,
        &eth::IVote::createProposalCall {
            targetModule: addresses::VAULT_ROUTER_ADDR,
            payload,
        },
        None,
    )?;
    ensure!(
        world.rpc.wait_successful_receipt(&proposal, RECEIPT_TRIES),
        "AddVault proposal {proposal} failed"
    );
    let proposal_id = world.rpc.proposal_id_from_receipt(port, &proposal)?;

    let ballots = voters
        .iter()
        .map(|validator| world.rpc.cast_vote(validator, proposal_id, true))
        .collect::<Result<Vec<_>>>()?;
    for ballot in &ballots {
        ensure!(
            world.rpc.wait_successful_receipt(ballot, RECEIPT_TRIES),
            "AddVault ballot {ballot} failed"
        );
    }
    wait_approved_vault_action(&url, proposal_id)?;

    let apply = eth::send_call(
        &url,
        addresses::VAULT_ROUTER_ADDR,
        &proposer,
        &eth::IVaultRouter::applyApprovedCall {
            proposalId: U256::from(proposal_id),
        },
        None,
    )?;
    ensure!(
        world.rpc.wait_successful_receipt(&apply, RECEIPT_TRIES),
        "applyApproved({proposal_id}) {apply} failed"
    );
    Ok(())
}

/// Wait until the tally records the approved AddVault of `proposal_id`.
fn wait_approved_vault_action(url: &str, proposal_id: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(APPROVAL_TIMEOUT_SECS);
    loop {
        let approved = eth::read_call(
            url,
            addresses::VAULT_ROUTER_ADDR,
            &eth::IVaultRouter::approvedVaultActionCall {
                proposalId: U256::from(proposal_id),
            },
        )
        .is_some_and(|action| matches!(action.action, eth::IVaultRouter::VaultAction::AddVault));
        if approved {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "AddVault proposal #{proposal_id} was not approved within {APPROVAL_TIMEOUT_SECS}s"
            ));
        }
        sleep(Duration::from_millis(250));
    }
}

/// The configured validators whose EVM keys are in the ACTIVE set.
fn active_validators(world: &World) -> Result<Vec<Validator>> {
    let active = world
        .rpc
        .active_validators(world.validators.primary_port())
        .ok_or_else(|| eyre!("read the ACTIVE validator set"))?;
    Ok((0..world.validators.size())
        .map(|index| world.validators.get(index))
        .filter(|validator| {
            validator
                .evm_key()
                .ok()
                .and_then(|key| eth::address_of(&key))
                .is_some_and(|address| active.contains(&address))
        })
        .collect())
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
