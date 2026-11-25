//! EVM-level integration test for `IVaultRouter.rebalance`.
//!
//! The vaultrouter crate's own unit tests stub every sub-call with a fixed
//! payload. So they cannot prove two things:
//!   * the four real sub-calls that `rebalance` makes (`asset.transferFrom`,
//!     `vault.deposit`, `vault.withdraw`, `asset.transfer`) actually dispatch
//!     through the EVM's own precompile routing.
//!   * a failure anywhere in that chain propagates as a revert of the whole
//!     call.
//!
//! This test drives the precompile through the actual EVM (`sub_call::run`,
//! which installs the outbe precompile set in the child frame). Two vaults
//! share one stubbed asset. The same-asset path of `rebalance` needs no
//! oracle or decimals lookup. So the counterparties can stay minimal raw
//! bytecode.
//!
//! What this pins that the unit tests cannot:
//!   * a real EOA-style caller reaches `rebalance` through the routed
//!     precompile dispatch, not just through `runtime::rebalance` directly.
//!   * a revert inside the destination asset's `transferFrom` (the caller
//!     has not approved) propagates all the way back as the outer call's
//!     failure, not a silently swallowed error.

use alloy_primitives::{keccak256, Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolError, SolEvent};
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, CacheDB, EmptyDB},
};
use outbe_evm::test_support::{seed_storage, EvmWorld};
use outbe_primitives::storage::{SubCallOutput, SubCallStatus};
use outbe_system_common::addresses::VAULT_ROUTER_ADDRESS;
use outbe_vaultrouter::precompile::IVaultRouter;
use outbe_vaultrouter::schema::VaultActionData;
use outbe_vaultrouter::VaultRouterContract;

/// The bonded CCA caller.
const CCA: Address = Address::new([0x11; 20]);
const ASSET: Address = Address::new([0x33; 20]);
const VAULT_FROM: Address = Address::new([0x55; 20]);
const VAULT_TO: Address = Address::new([0x77; 20]);

const AMOUNT: u64 = 1_000;

/// Minimal ERC-20 stub: returns 32 bytes of `1` for any calldata, i.e.
/// `true` for `transferFrom`/`transfer`.
///
/// `PUSH1 0x01, PUSH1 0x00, MSTORE, PUSH1 0x20, PUSH1 0x00, RETURN`
const ALWAYS_ONE: [u8; 10] = [0x60, 0x01, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3];

/// Reverts unconditionally with an empty reason, for any calldata:
/// `PUSH1 0x00, PUSH1 0x00, REVERT`.
const ALWAYS_REVERT: [u8; 5] = [0x60, 0x00, 0x60, 0x00, 0xfd];

fn code_account(code: &[u8]) -> AccountInfo {
    let bytecode = Bytecode::new_raw(Bytes::from(code.to_vec()));
    AccountInfo {
        code_hash: bytecode.hash_slow(),
        code: Some(bytecode),
        ..Default::default()
    }
}

/// A vault stub: `asset()` and every other selector (`previewWithdraw`,
/// `balanceOf`, `withdraw`, `deposit`) all answer with the same fixed
/// 32-byte word, encoding `asset`'s address. An address word doubles as a
/// (large but valid) `uint256`. So the same canned reply serves both the
/// asset lookup and every share-count return that this test does not
/// otherwise care about. The bytecode is `PUSH32 word, PUSH1 0x00, MSTORE,
/// PUSH1 0x20, PUSH1 0x00, RETURN`.
fn vault_account(asset: Address) -> AccountInfo {
    let mut code = Vec::with_capacity(34 + 8);
    code.push(0x7f); // PUSH32
    code.extend_from_slice(&asset.into_word().0);
    code.extend_from_slice(&[0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3]);
    code_account(&code)
}

/// A database with both vaults sharing `ASSET`. The function registers
/// `VAULT_FROM`/`VAULT_TO` with the router as production `addVault` would
/// leave them, unless `register_from`/`register_to` say otherwise. `asset_reverts`
/// swaps the asset stub for one that unconditionally reverts, simulating an
/// unapproved caller.
fn seeded_db(register_from: bool, register_to: bool, asset_reverts: bool) -> CacheDB<EmptyDB> {
    let mut database = CacheDB::new(EmptyDB::default());
    let asset_code: &[u8] = if asset_reverts {
        &ALWAYS_REVERT
    } else {
        &ALWAYS_ONE
    };
    database.insert_account_info(&ASSET, code_account(asset_code));
    database.insert_account_info(&VAULT_FROM, vault_account(ASSET));
    database.insert_account_info(&VAULT_TO, vault_account(ASSET));

    seed_storage(&mut database, 1, CCA, |storage| {
        storage
            .increase_balance(
                outbe_system_common::addresses::CCA_REGISTRY_ADDRESS,
                outbe_ccaregistry::constants::BOND_REQUIREMENT,
            )
            .unwrap();
        outbe_ccaregistry::runtime::bond(
            storage.clone(),
            CCA,
            outbe_ccaregistry::constants::BOND_REQUIREMENT,
            "Test CCA".into(),
        )
        .unwrap();
        let router = VaultRouterContract::new(storage.clone());
        if register_from {
            router.asset_vault_set(ASSET).insert(VAULT_FROM).unwrap();
        }
        if register_to {
            router.asset_vault_set(ASSET).insert(VAULT_TO).unwrap();
        }
    });
    database
}

fn rebalance_calldata(amount: U256, max_amount_to: U256) -> Bytes {
    Bytes::from(
        IVaultRouter::rebalanceCall {
            vaultFrom: VAULT_FROM,
            vaultTo: VAULT_TO,
            assetsMinor: amount,
            maxAssetsToMinor: max_amount_to,
        }
        .abi_encode(),
    )
}

fn rebalance(world: &mut EvmWorld) -> SubCallOutput {
    world.call(
        CCA,
        VAULT_ROUTER_ADDRESS,
        rebalance_calldata(U256::from(AMOUNT), U256::MAX),
        false,
    )
}

#[test]
fn rebalance_moves_stablecoins_between_two_vaults_through_real_frames() {
    let result = rebalance(&mut EvmWorld::new(
        seeded_db(true, true, false),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    ));
    assert!(
        matches!(result.status, SubCallStatus::Success),
        "rebalance must succeed, got {:?} returndata 0x{}",
        result.status,
        alloy_primitives::hex::encode(&result.returndata),
    );

    // Same asset prices 1:1: the router pulled exactly `AMOUNT`.
    let amount_to = IVaultRouter::rebalanceCall::abi_decode_returns(&result.returndata).unwrap();
    assert_eq!(amount_to, U256::from(AMOUNT));
}

/// Wave 5 N2 seam invariant: end-to-end EVM transaction where a Solidity-style caller
/// contract stores sentinel words in EVM memory, invokes `IVaultRouter.rebalance` (which
/// executes four nested EVM `sub_call`s that write into child memory), and reads back its
/// own memory after return — proving `evm2` precompile `sub_call` never clobbers caller memory.
#[test]
fn caller_memory_is_preserved_across_outbe_precompile_subcall() {
    use alloy_consensus::TxLegacy;
    use alloy_primitives::TxKind;
    use evm2::{env::BlockEnvExt, ethereum::TxEnvelope, SpecId};
    use outbe_evm::OutbeEvmFactory;
    use reth_ethereum::evm::EthEvmEnv;
    use reth_primitives_traits::Recovered;

    const EOA: Address = Address::new([0x99; 20]);
    let sentinels: [[u8; 32]; 4] = [[0x11; 32], [0x22; 32], [0x33; 32], [0x44; 32]];
    let post_ret_sentinel: [u8; 32] = [0x55; 32];

    // Build caller contract bytecode deployed at bonded `CCA`:
    // 1. MSTORE four 32-byte sentinels at 0x00, 0x20, 0x40, 0x60 and one above ret window at 0x220.
    // 2. CALLDATACOPY(0x100, 0x00, CALLDATASIZE).
    // 3. CALL(GAS, VAULT_ROUTER_ADDRESS, 0, 0x100, CALLDATASIZE, 0x200, 0x20).
    // 4. MSTORE(0x80, call_status); MSTORE(0xa0, MLOAD(0x200)); MSTORE(0xc0, MLOAD(0x220)).
    // 5. RETURN(0x00, 0xe0).
    let mut caller_code = Vec::new();
    for (idx, word) in sentinels.iter().enumerate() {
        caller_code.push(0x7f); // PUSH32
        caller_code.extend_from_slice(word);
        caller_code.extend_from_slice(&[0x60, (idx as u8) * 0x20, 0x52]); // PUSH1 offset; MSTORE
    }
    caller_code.push(0x7f); // PUSH32 post_ret_sentinel
    caller_code.extend_from_slice(&post_ret_sentinel);
    caller_code.extend_from_slice(&[0x61, 0x02, 0x20, 0x52]); // PUSH2 0x0220; MSTORE
    caller_code.extend_from_slice(&[
        0x36, // CALLDATASIZE
        0x60, 0x00, // PUSH1 0x00 (calldata offset)
        0x61, 0x01, 0x00, // PUSH2 0x0100 (dest memory offset)
        0x37, // CALLDATACOPY
        0x60, 0x20, // retSize = 0x20
        0x61, 0x02, 0x00, // retOffset = 0x0200
        0x36, // argsSize = CALLDATASIZE
        0x61, 0x01, 0x00, // argsOffset = 0x0100
        0x60, 0x00, // value = 0
        0x73, // PUSH20 VAULT_ROUTER_ADDRESS
    ]);
    caller_code.extend_from_slice(VAULT_ROUTER_ADDRESS.as_slice());
    caller_code.extend_from_slice(&[
        0x5a, // GAS
        0xf1, // CALL
        0x60, 0x80, // PUSH1 0x80
        0x52, // MSTORE call_status
        0x61, 0x02, 0x00, // PUSH2 0x0200
        0x51, // MLOAD amount_to
        0x60, 0xa0, // PUSH1 0xa0
        0x52, // MSTORE amount_to
        0x61, 0x02, 0x20, // PUSH2 0x0220
        0x51, // MLOAD post_ret_sentinel
        0x60, 0xc0, // PUSH1 0xc0
        0x52, // MSTORE post_ret_sentinel
        0x60, 0xe0, // PUSH1 0xe0 (224 bytes)
        0x60, 0x00, // PUSH1 0x00
        0xf3, // RETURN
    ]);

    let mut db = seeded_db(true, true, false);
    db.insert_account_info(&CCA, code_account(&caller_code));
    db.insert_account_info(
        &EOA,
        AccountInfo {
            balance: U256::from(10_000_000_000_000u64),
            ..Default::default()
        },
    );

    let env = EthEvmEnv::new(
        SpecId::PRAGUE,
        BlockEnvExt {
            number: U256::from(1u64),
            timestamp: U256::from(1u64),
            gas_limit: U256::from(30_000_000u64),
            ..Default::default()
        },
        outbe_primitives::chain::CHAIN_ID,
    );
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(outbe_primitives::chain::CHAIN_ID),
            nonce: 0,
            gas_price: 0,
            gas_limit: 5_000_000,
            to: TxKind::Call(CCA),
            value: U256::ZERO,
            input: rebalance_calldata(U256::from(AMOUNT), U256::MAX),
        }),
        EOA,
    );

    let mut evm = OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
        .create_evm(&mut db, env);
    let result = evm
        .transact(&tx)
        .expect("top-level transaction calling VaultRouter must execute")
        .commit();
    assert!(result.is_success(), "expected success, got {result:?}");
    assert_eq!(result.output.len(), 0xe0, "caller must return 7 ABI words");

    for (idx, expected) in sentinels.iter().enumerate() {
        let start = idx * 32;
        assert_eq!(
            &result.output[start..start + 32],
            expected,
            "caller memory word {idx} at offset {start:#x} was corrupted by Outbe precompile sub-call"
        );
    }
    assert_eq!(
        &result.output[0xc0..0xe0],
        &post_ret_sentinel,
        "caller memory sentinel above retSize (0x220) was overwritten by Outbe precompile sub-call"
    );
    let call_status = U256::from_be_slice(&result.output[0x80..0xa0]);
    let amount_to = U256::from_be_slice(&result.output[0xa0..0xc0]);
    assert_eq!(
        call_status,
        U256::from(1u64),
        "inner VaultRouter CALL must succeed"
    );
    assert_eq!(
        amount_to,
        U256::from(AMOUNT),
        "nested sub-calls must return rebalanced amount"
    );
}

#[test]
fn rebalance_reverts_atomically_when_the_caller_has_not_approved() {
    let result = rebalance(&mut EvmWorld::new(
        seeded_db(true, true, true),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    ));
    assert!(
        !matches!(result.status, SubCallStatus::Success),
        "a reverting transferFrom must fail the whole rebalance, got {:?}",
        result.status
    );
}

#[test]
fn rebalance_reverts_when_the_destination_vault_is_not_registered() {
    let result = rebalance(&mut EvmWorld::new(
        seeded_db(true, false, false),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    ));
    assert!(
        !matches!(result.status, SubCallStatus::Success),
        "an unregistered destination vault must not accept stablecoins, got {:?}",
        result.status
    );
}

sol! {
    interface ApprovalToken {
        function setFailure(uint256 fail) external;
        function allowance() external view returns (uint256);
    }
}

// Tiny bytecode fixtures keep this test independent of Solidity build artifacts.
fn selector_jump(code: &mut Vec<u8>, signature: &str) -> usize {
    code.extend_from_slice(&[0x60, 0x00, 0x35, 0x60, 0xe0, 0x1c, 0x63]);
    code.extend_from_slice(&keccak256(signature)[..4]);
    code.extend_from_slice(&[0x14, 0x61, 0, 0, 0x57]);
    code.len() - 3
}

fn label(code: &mut Vec<u8>, operand: usize) {
    let destination = u16::try_from(code.len()).unwrap().to_be_bytes();
    code[operand..operand + 2].copy_from_slice(&destination);
    code.push(0x5b);
}

fn return_word(code: &mut Vec<u8>, value: U256) {
    code.push(0x7f);
    code.extend_from_slice(&value.to_be_bytes::<32>());
    code.extend_from_slice(&[0x60, 0, 0x52, 0x60, 0x20, 0x60, 0, 0xf3]);
}

fn approval_vault() -> AccountInfo {
    let mut code = Vec::new();
    let owner = selector_jump(&mut code, "owner()");
    let balance = selector_jump(&mut code, "balanceOf(address)");
    return_word(&mut code, U256::from_be_slice(ASSET.as_slice()));
    label(&mut code, owner);
    return_word(&mut code, U256::ZERO);
    label(&mut code, balance);
    return_word(&mut code, U256::ZERO);
    code_account(&code)
}

/// One router-owned share per asset, stored in slot zero and changed only by
/// actual deposit/withdraw calls. Unknown selectors revert rather than masking
/// a missing fixture implementation with a canned return value.
fn stateful_vault() -> AccountInfo {
    let mut code = Vec::new();
    let asset = selector_jump(&mut code, "asset()");
    let balance = selector_jump(&mut code, "balanceOf(address)");
    let preview = selector_jump(&mut code, "previewWithdraw(uint256)");
    let deposit = selector_jump(&mut code, "deposit(uint256,address)");
    let withdraw = selector_jump(&mut code, "withdraw(uint256,address,address)");
    code.extend_from_slice(&ALWAYS_REVERT);
    label(&mut code, asset);
    return_word(&mut code, U256::from_be_slice(ASSET.as_slice()));
    label(&mut code, balance);
    code.extend_from_slice(&[0x60, 0, 0x54, 0x60, 0, 0x52, 0x60, 0x20, 0x60, 0, 0xf3]);
    label(&mut code, deposit);
    // SSTORE(0, SLOAD(0) + assets).
    code.extend_from_slice(&[0x60, 4, 0x35, 0x60, 0, 0x54, 0x01, 0x60, 0, 0x55]);
    code.extend_from_slice(&[0x61, 0, 0, 0x56]);
    let deposited = code.len() - 3;
    label(&mut code, withdraw);
    // SSTORE(0, SLOAD(0) - assets); rebalance checks sufficient shares first.
    code.extend_from_slice(&[0x60, 4, 0x35, 0x60, 0, 0x54, 0x03, 0x60, 0, 0x55]);
    label(&mut code, deposited);
    label(&mut code, preview);
    code.extend_from_slice(&[0x60, 4, 0x35, 0x60, 0, 0x52, 0x60, 0x20, 0x60, 0, 0xf3]);
    code_account(&code)
}

/// approve writes the allowance, emits a log, and calls applyVaultAction(1) again.
/// It requires the re-entry to fail, then optionally reverts based on slot 2.
fn approval_token() -> AccountInfo {
    let mut code = Vec::new();
    let iso = selector_jump(&mut code, "isoCode()");
    let approve = selector_jump(&mut code, "approve(address,uint256)");
    let set_failure = selector_jump(&mut code, "setFailure(uint256)");
    let transfer = selector_jump(&mut code, "transfer(address,uint256)");
    let transfer_from = selector_jump(&mut code, "transferFrom(address,address,uint256)");
    // Default getter: return the allowance stored in slot zero.
    code.extend_from_slice(&[0x60, 0, 0x54, 0x60, 0, 0x52, 0x60, 0x20, 0x60, 0, 0xf3]);
    label(&mut code, iso);
    return_word(&mut code, U256::from(840));
    label(&mut code, transfer);
    label(&mut code, transfer_from);
    return_word(&mut code, U256::ONE);
    label(&mut code, set_failure);
    code.extend_from_slice(&[0x60, 4, 0x35, 0x60, 2, 0x55, 0x00]);
    label(&mut code, approve);
    // SSTORE(0, amount); LOG0(0, 32).
    code.extend_from_slice(&[0x60, 0x24, 0x35, 0x60, 0, 0x55, 0x60, 0x20, 0x60, 0, 0xa0]);
    let mut selector = [0u8; 32];
    selector[..4].copy_from_slice(&IVaultRouter::applyVaultActionCall::SELECTOR);
    code.push(0x7f);
    code.extend_from_slice(&selector);
    code.extend_from_slice(&[0x60, 0, 0x52, 0x60, 1, 0x60, 4, 0x52]);
    // CALL(gas(), router, 0, 0, 36, 0, 0).
    code.extend_from_slice(&[0x60, 0, 0x60, 0, 0x60, 0x24, 0x60, 0, 0x60, 0, 0x73]);
    code.extend_from_slice(VAULT_ROUTER_ADDRESS.as_slice());
    code.extend_from_slice(&[0x5a, 0xf1, 0x15, 0x61, 0, 0, 0x57]);
    let reentry_rejected = code.len() - 3;
    code.extend_from_slice(&[0x60, 0, 0x60, 0, 0xfd]);
    label(&mut code, reentry_rejected);
    code.extend_from_slice(&[0x60, 2, 0x54, 0x15, 0x61, 0, 0, 0x57]);
    let succeeds = code.len() - 3;
    code.extend_from_slice(&[0x60, 0, 0x60, 0, 0xfd]);
    label(&mut code, succeeds);
    return_word(&mut code, U256::ONE);
    code_account(&code)
}

fn vault_action_db(fail: bool, kind: u8) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&ASSET, approval_token());
    db.insert_account_info(&VAULT_FROM, approval_vault());
    db.insert_account_storage(&ASSET, &U256::from(2), &U256::from(u8::from(fail)));
    seed_storage(&mut db, 1, CCA, |storage| {
        let router = VaultRouterContract::new(storage.clone());
        router
            .vault_actions
            .create(&VaultActionData {
                id: U256::ONE,
                vault: VAULT_FROM,
                kind,
                created_at: 0,
            })
            .unwrap();
        router
            .pending_vault_proposals
            .write(&VAULT_FROM, U256::ONE)
            .unwrap();
        if kind == 2 {
            router.assets.insert(ASSET).unwrap();
            router.asset_vault_set(ASSET).insert(VAULT_FROM).unwrap();
            router
                .reference_currency_vault_set(840)
                .insert(VAULT_FROM)
                .unwrap();
            router
                .vault_reference_currencies
                .write(&VAULT_FROM, 840)
                .unwrap();
            storage.sstore(ASSET, U256::ZERO, U256::MAX).unwrap();
        }
    });
    db
}

fn vault_action_world(fail: bool, kind: u8) -> EvmWorld {
    EvmWorld::new(
        vault_action_db(fail, kind),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
}

fn pending_removal_world() -> EvmWorld {
    let mut db = vault_action_db(false, 2);
    db.insert_account_info(&VAULT_FROM, stateful_vault());
    db.insert_account_info(&VAULT_TO, stateful_vault());
    db.insert_account_storage(&VAULT_FROM, &U256::ZERO, &U256::from(AMOUNT));
    seed_storage(&mut db, 1, CCA, |storage| {
        storage
            .increase_balance(
                outbe_system_common::addresses::CCA_REGISTRY_ADDRESS,
                outbe_ccaregistry::constants::BOND_REQUIREMENT,
            )
            .unwrap();
        outbe_ccaregistry::runtime::bond(
            storage.clone(),
            CCA,
            outbe_ccaregistry::constants::BOND_REQUIREMENT,
            "Test CCA".into(),
        )
        .unwrap();
        VaultRouterContract::new(storage)
            .asset_vault_set(ASSET)
            .insert(VAULT_TO)
            .unwrap();
    });
    EvmWorld::new(db, std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
}

fn apply_vault_action(world: &mut EvmWorld, is_static: bool) -> SubCallOutput {
    world.call(
        CCA,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::applyVaultActionCall {
            proposalId: U256::ONE,
        }
        .abi_encode()
        .into(),
        is_static,
    )
}

/// The router logs of `world`, in order. A successful apply has exactly two.
fn router_logs(world: &EvmWorld) -> Vec<alloy_primitives::LogData> {
    let logs: Vec<_> = world
        .logs()
        .iter()
        .filter(|log| log.address == VAULT_ROUTER_ADDRESS)
        .map(|log| log.data.clone())
        .collect();
    assert_eq!(logs.len(), 2);
    logs
}

#[test]
fn vault_action_add_rolls_back_real_child_writes_logs_and_retries_with_reentry_denied() {
    let mut world = vault_action_world(true, 1);
    assert!(!matches!(
        apply_vault_action(&mut world, false).status,
        SubCallStatus::Success
    ));
    assert_eq!(
        world.view(CCA, ASSET, ApprovalToken::allowanceCall {}),
        U256::ZERO
    );
    assert_eq!(
        world.view(CCA, VAULT_ROUTER_ADDRESS, IVaultRouter::assetsCountCall {}),
        U256::ZERO
    );
    assert_eq!(
        world.view(
            CCA,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::referenceCurrencyVaultsCountCall { isoCode: 840 }
        ),
        U256::ZERO
    );
    assert_eq!(
        world.view(
            CCA,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::vaultReferenceCurrencyCall { vault: VAULT_FROM }
        ),
        0
    );
    assert!(world.logs().is_empty());

    world.ok(
        CCA,
        ASSET,
        ApprovalToken::setFailureCall { fail: U256::ZERO },
    );
    assert!(matches!(
        apply_vault_action(&mut world, false).status,
        SubCallStatus::Success
    ));
    assert_eq!(
        world.view(CCA, ASSET, ApprovalToken::allowanceCall {}),
        U256::MAX
    );
    assert_eq!(
        world.view(
            CCA,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::assetVaultsCountCall { asset: ASSET }
        ),
        U256::ONE
    );
    // Token log, VaultActionApplied and VaultAdded. No re-entry effects.
    assert_eq!(world.logs().len(), 3);
    let router_logs = router_logs(&world);
    IVaultRouter::VaultActionApplied::decode_log_data(&router_logs[0]).unwrap();
    IVaultRouter::VaultAdded::decode_log_data(&router_logs[1]).unwrap();
    assert!(!matches!(
        apply_vault_action(&mut world, false).status,
        SubCallStatus::Success
    ));
}

#[test]
fn vault_action_remove_rolls_back_then_clears_real_allowance_and_indexes() {
    let mut world = vault_action_world(true, 2);
    assert!(!matches!(
        apply_vault_action(&mut world, false).status,
        SubCallStatus::Success
    ));
    assert_eq!(
        world.view(CCA, ASSET, ApprovalToken::allowanceCall {}),
        U256::MAX
    );
    assert_eq!(
        world.view(CCA, VAULT_ROUTER_ADDRESS, IVaultRouter::assetsCountCall {}),
        U256::ONE
    );
    assert_eq!(
        world.view(
            CCA,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::referenceCurrencyVaultsCountCall { isoCode: 840 }
        ),
        U256::ONE
    );
    assert!(world.logs().is_empty());
    world.ok(
        CCA,
        ASSET,
        ApprovalToken::setFailureCall { fail: U256::ZERO },
    );
    assert!(matches!(
        apply_vault_action(&mut world, false).status,
        SubCallStatus::Success
    ));
    assert_eq!(
        world.view(CCA, ASSET, ApprovalToken::allowanceCall {}),
        U256::ZERO
    );
    assert_eq!(
        world.view(CCA, VAULT_ROUTER_ADDRESS, IVaultRouter::assetsCountCall {}),
        U256::ZERO
    );
    assert_eq!(
        world.view(
            CCA,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::referenceCurrencyVaultsCountCall { isoCode: 840 }
        ),
        U256::ZERO
    );
    assert_eq!(world.logs().len(), 3);
    let router_logs = router_logs(&world);
    IVaultRouter::VaultActionApplied::decode_log_data(&router_logs[0]).unwrap();
    IVaultRouter::VaultRemoved::decode_log_data(&router_logs[1]).unwrap();
    assert!(!matches!(
        apply_vault_action(&mut world, false).status,
        SubCallStatus::Success
    ));
}

#[test]
fn vault_action_apply_rejects_static_frame_without_consuming_action() {
    let mut world = vault_action_world(false, 1);
    assert!(!matches!(
        apply_vault_action(&mut world, true).status,
        SubCallStatus::Success
    ));
    assert!(world.logs().is_empty());
    assert!(matches!(
        apply_vault_action(&mut world, false).status,
        SubCallStatus::Success
    ));
}

#[test]
fn vault_action_remove_retries_after_active_cca_drains_real_vault_shares() {
    let mut world = pending_removal_world();
    let shares = |world: &mut EvmWorld, vault| {
        world.view(
            CCA,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::sharesBalanceCall { vault },
        )
    };
    assert_eq!(shares(&mut world, VAULT_FROM), U256::from(AMOUNT));
    assert_eq!(shares(&mut world, VAULT_TO), U256::ZERO);
    let result = apply_vault_action(&mut world, false);
    assert!(matches!(result.status, SubCallStatus::Revert(_)));
    let error = IVaultRouter::ReserveVaultHasShares::abi_decode(&result.returndata).unwrap();
    assert_eq!(error.vault, VAULT_FROM);
    assert_eq!(error.shares, U256::from(AMOUNT));
    let pending = world.view(
        CCA,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::getVaultActionCall {
            proposalId: U256::ONE,
        },
    );
    assert!(matches!(
        pending.action,
        IVaultRouter::VaultAction::RemoveVault
    ));
    assert_eq!(pending.vault, VAULT_FROM);
    assert_eq!(pending.createdAt, 0);
    assert_eq!(
        world.view(CCA, ASSET, ApprovalToken::allowanceCall {}),
        U256::MAX
    );
    assert_eq!(
        world.view(
            CCA,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::assetVaultsCountCall { asset: ASSET }
        ),
        U256::from(2)
    );
    assert_eq!(
        world.view(
            CCA,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::assetVaultAtCall {
                asset: ASSET,
                index: U256::ZERO
            }
        ),
        VAULT_FROM
    );
    assert!(world.logs().is_empty());

    let moved = rebalance(&mut world);
    assert!(matches!(moved.status, SubCallStatus::Success));
    assert_eq!(
        IVaultRouter::rebalanceCall::abi_decode_returns(&moved.returndata).unwrap(),
        U256::from(AMOUNT)
    );
    assert_eq!(shares(&mut world, VAULT_FROM), U256::ZERO);
    assert_eq!(shares(&mut world, VAULT_TO), U256::from(AMOUNT));
    assert_eq!(world.logs().len(), 1);
    let event = IVaultRouter::StablecoinRebalanced::decode_log_data(&world.logs()[0].data).unwrap();
    assert_eq!(event.cca, CCA);
    assert_eq!(event.burnedShares, U256::from(AMOUNT));
    assert_eq!(event.mintedShares, U256::from(AMOUNT));

    assert!(matches!(
        apply_vault_action(&mut world, false).status,
        SubCallStatus::Success
    ));
    let pending = world.view(
        CCA,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::getVaultActionCall {
            proposalId: U256::ONE,
        },
    );
    assert!(matches!(pending.action, IVaultRouter::VaultAction::Unknown));
    assert_eq!(
        world.view(CCA, ASSET, ApprovalToken::allowanceCall {}),
        U256::ZERO
    );
    assert_eq!(
        world.view(
            CCA,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::assetVaultsCountCall { asset: ASSET }
        ),
        U256::ONE
    );
    // Rebalance, VaultActionApplied, allowance revocation, removal.
    assert_eq!(world.logs().len(), 4);
    let applied = IVaultRouter::VaultActionApplied::decode_log_data(&world.logs()[1].data).unwrap();
    assert_eq!((applied.proposalId, applied.vault), (U256::ONE, VAULT_FROM));
    assert_eq!(
        world.view(
            CCA,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::assetVaultAtCall {
                asset: ASSET,
                index: U256::ZERO
            }
        ),
        VAULT_TO
    );
    let removed = IVaultRouter::VaultRemoved::decode_log_data(&world.logs()[3].data).unwrap();
    assert_eq!(removed.vault, VAULT_FROM);
    assert!(!matches!(
        apply_vault_action(&mut world, false).status,
        SubCallStatus::Success
    ));
}

#[test]
fn reserve_stables_rejects_pending_first_vault_removal_through_abi() {
    let mut world = pending_removal_world();
    let result = world.call(
        CCA,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::createCredisReservationCall {
            credisOwner: CCA,
            source: CCA,
            asset: ASSET,
            amountMinor: U256::from(AMOUNT),
            referenceCurrency: 840,
        }
        .abi_encode()
        .into(),
        false,
    );
    assert!(matches!(result.status, SubCallStatus::Revert(_)));
    let error = IVaultRouter::ReserveVaultRemovalPending::abi_decode(&result.returndata).unwrap();
    assert_eq!(error.vault, VAULT_FROM);
    assert_eq!(error.proposalId, U256::ONE);
    assert_eq!(
        world.view(
            CCA,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::sharesBalanceCall { vault: VAULT_FROM }
        ),
        U256::from(AMOUNT)
    );
    let reservation = world.view(
        CCA,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::getCredisReservationCall {
            credisReservationId: U256::ONE,
        },
    );
    assert_eq!(reservation.amountMinor, U256::ZERO);
    assert!(world.logs().is_empty());
}
