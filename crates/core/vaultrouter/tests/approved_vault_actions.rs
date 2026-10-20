//! EVM-level integration test for vault changes approved by a validator vote.
//!
//! The vote target records an approved vault action in the begin-block hook,
//! where sub-calls are not available. This test records it the same way, on a
//! `DirectStorageProvider` without sub-calls. Then an ordinary caller sends
//! `applyApproved` through the real EVM, where `add_vault` and `remove_vault`
//! make their sub-calls into the vault and the asset.
//!
//! The asset stub stores every `approve(spender, amount)` and answers
//! `allowance(owner, spender)` from that storage. So the test reads the real
//! allowance that the router gave the vault.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use outbe_evm::test_support::{seed_storage, EvmWorld};
use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::chain::CHAIN_ID;
use outbe_primitives::storage::SubCallStatus;
use outbe_vaultrouter::api::IVaultRouter;
use outbe_vaultrouter::{VaultRouterContract, VaultRouterVoteAction, VaultRouterVoteTarget};
use outbe_vote::handlers::{TargetExecutionOutcome, VoteTarget, VoteTargetContext};
use revm::{
    database::{CacheDB, EmptyDB},
    state::{AccountInfo, Bytecode},
};

sol! {
    interface ITestAsset {
        function approve(address spender, uint256 amount) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
    }

    interface ITestVault {
        function asset() external view returns (address);
    }
}

const ASSET: Address = Address::new([0x33; 20]);
const VAULT: Address = Address::new([0x55; 20]);
const ANYONE: Address = Address::new([0x99; 20]);
const USD_ISO_CODE: u16 = 840;
const TIMESTAMP: u64 = 1_700_000_000;

fn code_account(code: Vec<u8>) -> AccountInfo {
    let bytecode = Bytecode::new_raw(Bytes::from(code));
    AccountInfo {
        code_hash: bytecode.hash_slow(),
        code: Some(bytecode),
        ..Default::default()
    }
}

/// `RETURN` of the 32-byte word on top of the stack.
const RETURN_TOP: [u8; 8] = [0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3];
/// Pushes the 4-byte selector of the calldata.
const LOAD_SELECTOR: [u8; 6] = [0x60, 0x00, 0x35, 0x60, 0xe0, 0x1c];

/// Bytecode that jumps to `branch` when the selector on the stack equals
/// `selector`. The selector stays on the stack.
fn jump_if(selector: [u8; 4], branch: u8) -> Vec<u8> {
    let mut code = vec![0x80, 0x63];
    code.extend_from_slice(&selector);
    code.extend_from_slice(&[0x14, 0x60, branch, 0x57]);
    code
}

/// Asset stub. `approve(spender, amount)` stores `amount` at slot `spender`.
/// `allowance(owner, spender)` returns slot `spender`. Every other call returns
/// the ISO code 840 (USD).
fn asset_code() -> Vec<u8> {
    // Head: selector load, two conditional jumps (10 bytes each), default branch.
    let default_branch: Vec<u8> = [&[0x61, 0x03, 0x48][..], &RETURN_TOP].concat();
    let approve_at = (LOAD_SELECTOR.len() + 20 + default_branch.len()) as u8;
    // JUMPDEST, amount, spender, SSTORE, return true.
    let approve_branch: Vec<u8> = [
        &[0x5b, 0x60, 0x24, 0x35, 0x60, 0x04, 0x35, 0x55, 0x60, 0x01][..],
        &RETURN_TOP,
    ]
    .concat();
    let allowance_at = approve_at + approve_branch.len() as u8;
    // JUMPDEST, spender, SLOAD, return it.
    let allowance_branch: Vec<u8> = [&[0x5b, 0x60, 0x24, 0x35, 0x54][..], &RETURN_TOP].concat();
    [
        LOAD_SELECTOR.to_vec(),
        jump_if(ITestAsset::approveCall::SELECTOR, approve_at),
        jump_if(ITestAsset::allowanceCall::SELECTOR, allowance_at),
        default_branch,
        approve_branch,
        allowance_branch,
    ]
    .concat()
}

/// Ownerless vault stub of `ASSET`: `asset()` returns `ASSET`. Every other call
/// (`owner()`) returns zero.
fn vault_code() -> Vec<u8> {
    let default_branch: Vec<u8> = [&[0x60, 0x00][..], &RETURN_TOP].concat();
    let asset_at = (LOAD_SELECTOR.len() + 10 + default_branch.len()) as u8;
    let mut asset_branch = vec![0x5b, 0x73];
    asset_branch.extend_from_slice(ASSET.as_slice());
    asset_branch.extend_from_slice(&RETURN_TOP);
    [
        LOAD_SELECTOR.to_vec(),
        jump_if(ITestVault::assetCall::SELECTOR, asset_at),
        default_branch,
        asset_branch,
    ]
    .concat()
}

/// A database with the asset and vault stubs, where the vote of `proposal_id`
/// approved `action` at block 1. The approval runs on a provider without
/// sub-calls, as in the begin-block hook.
fn approved_db(
    proposal_id: u64,
    action: VaultRouterVoteAction,
    seed: impl FnOnce(&mut CacheDB<EmptyDB>),
) -> CacheDB<EmptyDB> {
    let mut database = CacheDB::new(EmptyDB::default());
    database.insert_account_info(ASSET, code_account(asset_code()));
    database.insert_account_info(VAULT, code_account(vault_code()));
    seed(&mut database);
    let outcome = seed_storage(&mut database, TIMESTAMP, ANYONE, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, TIMESTAMP, CHAIN_ID),
            storage,
        );
        VaultRouterVoteTarget
            .handle_approved(
                &ctx,
                U256::from(proposal_id),
                action.to_json().as_bytes(),
                VoteTargetContext {
                    proposer: ANYONE,
                    attached_value: U256::ZERO,
                    block_number: 1,
                    chain_id: CHAIN_ID,
                },
            )
            .unwrap()
    });
    assert_eq!(outcome, TargetExecutionOutcome::Applied);
    database
}

fn add_vault() -> VaultRouterVoteAction {
    VaultRouterVoteAction::AddVault {
        vault: VAULT,
        asset: ASSET,
        iso_code: USD_ISO_CODE,
    }
}

fn apply(world: &mut EvmWorld, proposal_id: u64) -> SubCallStatus {
    world
        .call(
            ANYONE,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::applyApprovedCall {
                proposalId: U256::from(proposal_id),
            }
            .abi_encode()
            .into(),
            false,
        )
        .status
}

fn allowance(world: &mut EvmWorld) -> U256 {
    world.view(
        ANYONE,
        ASSET,
        ITestAsset::allowanceCall {
            owner: VAULT_ROUTER_ADDRESS,
            spender: VAULT,
        },
    )
}

fn asset_vaults_count(world: &mut EvmWorld) -> U256 {
    world.view(
        ANYONE,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall { asset: ASSET },
    )
}

fn logged<E: SolEvent>(world: &EvmWorld) -> usize {
    world
        .logs()
        .iter()
        .filter(|log| log.address == VAULT_ROUTER_ADDRESS && E::decode_log(log).is_ok())
        .count()
}

#[test]
fn apply_approved_add_vault_grants_the_vault_an_unlimited_allowance() {
    let mut world = EvmWorld::new(approved_db(1, add_vault(), |_| {}));
    assert_eq!(allowance(&mut world), U256::ZERO);

    let status = apply(&mut world, 1);
    assert!(matches!(status, SubCallStatus::Success), "{status:?}");
    assert_eq!(allowance(&mut world), U256::MAX);
    assert_eq!(asset_vaults_count(&mut world), U256::ONE);
    assert_eq!(logged::<IVaultRouter::VaultAdded>(&world), 1);

    // The action is single-use.
    let status = apply(&mut world, 1);
    assert!(!matches!(status, SubCallStatus::Success), "{status:?}");
}

#[test]
fn apply_approved_remove_vault_revokes_the_allowance() {
    let database = approved_db(
        2,
        VaultRouterVoteAction::RemoveVault { vault: VAULT },
        |database| {
            // The vault as an applied AddVault leaves it.
            seed_storage(database, TIMESTAMP, ANYONE, |storage| {
                let router = VaultRouterContract::new(storage);
                router.assets.insert(ASSET).unwrap();
                router.asset_vault_set(ASSET).insert(VAULT).unwrap();
                router
                    .reference_currency_vault_set(USD_ISO_CODE)
                    .insert(VAULT)
                    .unwrap();
                router
                    .vault_reference_currencies
                    .write(&VAULT, USD_ISO_CODE)
                    .unwrap();
            });
            database
                .insert_account_storage(ASSET, U256::from_be_bytes(VAULT.into_word().0), U256::MAX)
                .unwrap();
        },
    );
    let mut world = EvmWorld::new(database);
    assert_eq!(allowance(&mut world), U256::MAX);

    let status = apply(&mut world, 2);
    assert!(matches!(status, SubCallStatus::Success), "{status:?}");
    assert_eq!(allowance(&mut world), U256::ZERO);
    assert_eq!(asset_vaults_count(&mut world), U256::ZERO);
    assert_eq!(logged::<IVaultRouter::VaultRemoved>(&world), 1);
}

#[test]
fn apply_approved_after_expiry_clears_the_action_without_a_sub_call() {
    let mut world = EvmWorld::new(approved_db(3, add_vault(), |_| {}));
    let window = outbe_chain_constants::get_governance_voting_window_blocks();
    world.ctx.block.number = U256::from(1 + window + 1);

    let status = apply(&mut world, 3);
    assert!(matches!(status, SubCallStatus::Success), "{status:?}");
    assert_eq!(allowance(&mut world), U256::ZERO);
    assert_eq!(asset_vaults_count(&mut world), U256::ZERO);
    assert_eq!(logged::<IVaultRouter::VaultActionExpired>(&world), 1);
    assert_eq!(logged::<IVaultRouter::VaultAdded>(&world), 0);

    let status = apply(&mut world, 3);
    assert!(!matches!(status, SubCallStatus::Success), "{status:?}");
}
