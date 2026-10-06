//! EVM-level integration test for `IPayNote.deposit`.
//!
//! The paynote crate's own tests can only cover `deposit`'s pre-mutation
//! guards. Its body performs three real sub-calls that an in-memory storage
//! provider cannot serve: `asset.transferFrom`, `asset.approve`, and
//! `VaultRouter.deposit`. This test drives the precompile through the actual
//! EVM (`sub_call::run`, which installs the outbe precompile set in the child
//! frame). Thus those sub-calls dispatch for real. The VaultRouter precompile
//! runs its own liquidity-source gating and vault lookup. The ERC20/ERC4626
//! counterparties are the stateful `FactorySettlement` fixture.
//!
//! What this pins that unit tests cannot:
//!   * `PAYNOTE_ADDRESS` must be a registered VaultRouter liquidity source.
//!     The `PayNoteDeposit` discriminant seeded at genesis is load-bearing.
//!   * the asset must have a registered reserve vault.
//!   * a revert anywhere in that chain rolls the tree back atomically.
//!   * the appended leaf is the runtime-derived commitment, readable through
//!     the public view ABI.

mod sub_call_support;

use outbe_paynote::Field;
use outbe_protocol::codec::field_to_b256;

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_evm::sub_call;
use outbe_paynote::hash::{note_commitment, note_sn};
use outbe_paynote::precompile::IPayNote;
use outbe_primitives::addresses::{PAYNOTE_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::{
    block::BlockContext,
    storage::{direct::DirectStorageProvider, StorageHandle, SubCallInput, SubCallStatus},
};
use outbe_vaultrouter::VaultRouterContract;
use revm::{
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
    Context,
};

sol! {
    interface IFixture {
        function mint(address account, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
        function configure(uint256 mode, address factory, bytes reentry) external;
    }
}

const ALICE: Address = Address::new([0x11; 20]);
const ASSET: Address = Address::new([0x33; 20]);
const VAULT: Address = Address::new([0x55; 20]);
const UNREGISTERED_ASSET: Address = Address::new([0x66; 20]);

/// `StablesSource::PayNoteDeposit` — the discriminant `seed_genesis.py`
/// registers for `PAYNOTE_ADDRESS`.
const PAYNOTE_DEPOSIT_SOURCE: u8 = 4;

const DEPOSIT_AMOUNT: u128 = 1_000;
const SPEND_KEY: u64 = 17;

/// The stateful token and vault: balances and allowances move for real.
fn fixture_account() -> AccountInfo {
    let bytecode = Bytecode::new_raw(Bytes::from(
        alloy_primitives::hex::decode(include_str!("fixtures/FactorySettlement.hex").trim())
            .unwrap(),
    ));
    AccountInfo {
        code_hash: bytecode.hash_slow(),
        code: Some(bytecode),
        ..Default::default()
    }
}

fn block() -> BlockContext {
    BlockContext::new(1, 1, outbe_primitives::chain::CHAIN_ID, ALICE, vec![ALICE])
}

/// The commitment the runtime must derive for a deposit of `amount` of
/// `asset` under `SPEND_KEY`'s serial — computed independently here.
fn expected_commitment(asset: Address, amount: u128) -> Field {
    expected_commitment_u256(asset, U256::from(amount))
}

fn expected_commitment_u256(asset: Address, amount: U256) -> Field {
    let serial = note_sn(Field::from(SPEND_KEY)).unwrap();
    note_commitment(outbe_primitives::chain::CHAIN_ID, serial, asset, amount).unwrap()
}

fn note_serial_word() -> alloy_primitives::B256 {
    field_to_b256(&note_sn(Field::from(SPEND_KEY)).unwrap()).unwrap()
}

/// A database with the counterparty fixture deployed and VaultRouter seeded
/// as production genesis would seed it. VaultRouter has a vault registered for
/// `ASSET`. Paynote is an authorized `PayNoteDeposit` liquidity source unless
/// `authorize_paynote` says otherwise.
fn seeded_db(register_vault: bool, authorize_paynote: bool) -> CacheDB<EmptyDB> {
    let mut database = CacheDB::new(EmptyDB::default());
    database.insert_account_info(ASSET, fixture_account());
    database.insert_account_info(UNREGISTERED_ASSET, fixture_account());
    database.insert_account_info(VAULT, fixture_account());

    let mut provider = DirectStorageProvider::new(&mut database, block());
    StorageHandle::enter(&mut provider, |storage| {
        let router = VaultRouterContract::new(storage.clone());
        if register_vault {
            router.assets.insert(ASSET).unwrap();
            router.asset_vault_set(ASSET).insert(VAULT).unwrap();
        }
        if authorize_paynote {
            router.liquidity_sources.insert(PAYNOTE_ADDRESS).unwrap();
            router
                .liquidity_source_types
                .write(&PAYNOTE_ADDRESS, PAYNOTE_DEPOSIT_SOURCE)
                .unwrap();
        }
    });
    provider.flush().unwrap();
    database
}

fn deposit_calldata(asset: Address, amount: u128) -> Bytes {
    deposit_calldata_u256(asset, U256::from(amount))
}

fn deposit_calldata_u256(asset: Address, amount: U256) -> Bytes {
    Bytes::from(
        IPayNote::depositCall {
            asset,
            amount,
            noteSn: note_serial_word(),
        }
        .abi_encode(),
    )
}

macro_rules! run_call {
    ($ctx:expr, $target:expr, $calldata:expr, $is_static:expr) => {
        run_call!($ctx, ALICE, $target, $calldata, $is_static)
    };
    ($ctx:expr, $caller:expr, $target:expr, $calldata:expr, $is_static:expr) => {
        sub_call::run(
            $ctx,
            sub_call_support::fresh_environment($caller, SpecId::PRAGUE),
            SubCallInput {
                target: $target,
                value: U256::ZERO,
                calldata: $calldata,
                gas_limit: 5_000_000,
                is_static: $is_static,
            },
        )
        .expect("sub-call must not fail fatally")
    };
}

type EvmCtx = revm::Context<
    revm::context::BlockEnv,
    revm::context::TxEnv,
    revm::context::CfgEnv,
    CacheDB<EmptyDB>,
>;

/// The EVM context the sub-call runs in. `Context::mainnet()` defaults to
/// chain id 1. The runtime folds the live chain id into every note
/// commitment, so the test would otherwise derive a leaf for the wrong chain.
///
/// `ALICE` holds and has approved both assets to the pool, and the router has
/// approved the vault, as a live deployment would.
fn evm_ctx(db: CacheDB<EmptyDB>) -> EvmCtx {
    let mut ctx = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| cfg.chain_id = outbe_primitives::chain::CHAIN_ID);
    for asset in [ASSET, UNREGISTERED_ASSET] {
        fixture_call(
            &mut ctx,
            ALICE,
            asset,
            IFixture::mintCall {
                account: ALICE,
                amount: U256::MAX,
            },
        );
        fixture_call(
            &mut ctx,
            ALICE,
            asset,
            IFixture::approveCall {
                spender: PAYNOTE_ADDRESS,
                amount: U256::MAX,
            },
        );
    }
    fixture_call(
        &mut ctx,
        VAULT_ROUTER_ADDRESS,
        ASSET,
        IFixture::approveCall {
            spender: VAULT,
            amount: U256::MAX,
        },
    );
    ctx
}

fn fixture_call(ctx: &mut EvmCtx, caller: Address, target: Address, call: impl SolCall) {
    let out = run_call!(ctx, caller, target, Bytes::from(call.abi_encode()), false);
    assert!(
        matches!(out.status, SubCallStatus::Success),
        "fixture setup reverted: {:?}",
        out.status
    );
}

/// The tree must be untouched. A VaultRouter revert has to roll the whole
/// deposit back, leaf and all. It must not leave a commitment behind for value
/// that never reached a vault.
macro_rules! assert_pristine {
    ($ctx:expr) => {{
        let count = run_call!(
            $ctx,
            PAYNOTE_ADDRESS,
            Bytes::from(IPayNote::leafCountCall {}.abi_encode()),
            true
        );
        assert_eq!(
            IPayNote::leafCountCall::abi_decode_returns(&count.returndata).unwrap(),
            0,
            "a failed deposit must leave no leaf behind"
        );
    }};
}

#[test]
fn deposit_routes_full_width_amount_through_vault_router_and_appends_commitment() {
    let mut ctx = evm_ctx(seeded_db(true, true));
    let amount = (U256::from(1) << 200) + U256::from(DEPOSIT_AMOUNT);

    let result = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        deposit_calldata_u256(ASSET, amount),
        false
    );
    assert!(
        matches!(result.status, SubCallStatus::Success),
        "deposit must succeed, got {:?} returndata 0x{}",
        result.status,
        alloy_primitives::hex::encode(&result.returndata),
    );

    // The tree advanced by exactly one leaf, read back through the public ABI.
    let count = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        Bytes::from(IPayNote::leafCountCall {}.abi_encode()),
        true
    );
    assert_eq!(
        IPayNote::leafCountCall::abi_decode_returns(&count.returndata).unwrap(),
        1
    );

    // And the appended leaf is the commitment the runtime derived from the
    // asset and amount it actually moved — not anything the caller supplied.
    let commitment = field_to_b256(&expected_commitment_u256(ASSET, amount)).unwrap();
    let present = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        Bytes::from(IPayNote::hasCommitmentCall { commitment }.abi_encode()),
        true
    );
    assert!(
        IPayNote::hasCommitmentCall::abi_decode_returns(&present.returndata).unwrap(),
        "the derived commitment must be a leaf of the tree"
    );

    // The post-deposit root is inside the acceptance window, so a proof built
    // against it right now would be spendable.
    let root = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        Bytes::from(IPayNote::currentRootCall {}.abi_encode()),
        true
    );
    let root = IPayNote::currentRootCall::abi_decode_returns(&root.returndata).unwrap();
    let known = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        Bytes::from(IPayNote::isKnownRootCall { root }.abi_encode()),
        true
    );
    assert!(IPayNote::isKnownRootCall::abi_decode_returns(&known.returndata).unwrap());
}

#[test]
fn deposit_reverts_and_leaves_no_leaf_when_paynote_is_not_a_liquidity_source() {
    // Genesis authorization is load-bearing: without the `PayNoteDeposit`
    // source registration, VaultRouter rejects the routed deposit.
    let mut ctx = evm_ctx(seeded_db(true, false));

    let result = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        deposit_calldata(ASSET, DEPOSIT_AMOUNT),
        false
    );
    assert!(
        !matches!(result.status, SubCallStatus::Success),
        "an unauthorized liquidity source must not deposit"
    );

    assert_pristine!(&mut ctx);
}

#[test]
fn deposit_reverts_and_leaves_no_leaf_when_the_asset_has_no_vault() {
    let mut ctx = evm_ctx(seeded_db(false, true));

    let result = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        deposit_calldata(UNREGISTERED_ASSET, DEPOSIT_AMOUNT),
        false
    );
    assert!(
        !matches!(result.status, SubCallStatus::Success),
        "an asset without a reserve vault must not deposit"
    );

    assert_pristine!(&mut ctx);
}

#[test]
fn a_second_identical_deposit_reverts_on_the_duplicate_leaf() {
    // Dedup is on the leaf, not the serial. A re-deposit of the same amount of
    // the same asset under the same serial rebuilds the identical commitment.
    // That commitment would alias one nullifier onto two notes and lock one of
    // them forever.
    let mut ctx = evm_ctx(seeded_db(true, true));

    let first = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        deposit_calldata(ASSET, DEPOSIT_AMOUNT),
        false
    );
    assert!(matches!(first.status, SubCallStatus::Success));

    let second = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        deposit_calldata(ASSET, DEPOSIT_AMOUNT),
        false
    );
    assert!(
        !matches!(second.status, SubCallStatus::Success),
        "a duplicate commitment must be rejected"
    );

    // Still exactly the one leaf from the first deposit.
    let count = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        Bytes::from(IPayNote::leafCountCall {}.abi_encode()),
        true
    );
    assert_eq!(
        IPayNote::leafCountCall::abi_decode_returns(&count.returndata).unwrap(),
        1
    );
}

#[test]
fn a_differing_amount_under_the_same_serial_is_a_distinct_leaf() {
    // The serial is amount-independent, so the same spend key can legitimately
    // fund several notes; each amount must produce its own leaf.
    let mut ctx = evm_ctx(seeded_db(true, true));

    for amount in [DEPOSIT_AMOUNT, DEPOSIT_AMOUNT + 1] {
        let result = run_call!(
            &mut ctx,
            PAYNOTE_ADDRESS,
            deposit_calldata(ASSET, amount),
            false
        );
        assert!(
            matches!(result.status, SubCallStatus::Success),
            "deposit of {amount} must succeed, got {:?}",
            result.status
        );
    }

    let count = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        Bytes::from(IPayNote::leafCountCall {}.abi_encode()),
        true
    );
    assert_eq!(
        IPayNote::leafCountCall::abi_decode_returns(&count.returndata).unwrap(),
        2
    );

    for amount in [DEPOSIT_AMOUNT, DEPOSIT_AMOUNT + 1] {
        let commitment = field_to_b256(&expected_commitment(ASSET, amount)).unwrap();
        let present = run_call!(
            &mut ctx,
            PAYNOTE_ADDRESS,
            Bytes::from(IPayNote::hasCommitmentCall { commitment }.abi_encode()),
            true
        );
        assert!(
            IPayNote::hasCommitmentCall::abi_decode_returns(&present.returndata).unwrap(),
            "the leaf for amount {amount} must be present"
        );
    }
}

fn asset_balances(ctx: &mut EvmCtx) -> [U256; 4] {
    [ALICE, PAYNOTE_ADDRESS, VAULT_ROUTER_ADDRESS, VAULT].map(|account| {
        let out = run_call!(
            ctx,
            ASSET,
            Bytes::from(IFixture::balanceOfCall { account }.abi_encode()),
            true
        );
        IFixture::balanceOfCall::abi_decode_returns(&out.returndata).unwrap()
    })
}

fn configure_asset(ctx: &mut EvmCtx, mode: u64) {
    fixture_call(
        ctx,
        ALICE,
        ASSET,
        IFixture::configureCall {
            mode: U256::from(mode),
            factory: PAYNOTE_ADDRESS,
            reentry: Bytes::new(),
        },
    );
}

#[test]
fn a_token_that_does_not_deliver_the_amount_deposits_nothing() {
    // False transferFrom, false approve, malformed bool, success without
    // movement, and fee-on-transfer.
    for (mode, reason) in [
        (1, "PayNote token call failed"),
        (2, "PayNote token call failed"),
        (6, "PayNote token call failed"),
        (8, "PayNote token moved an unexpected amount"),
        (9, "PayNote token moved an unexpected amount"),
    ] {
        let mut ctx = evm_ctx(seeded_db(true, true));
        // Stray tokens in the pool must never pay for a deposit.
        fixture_call(
            &mut ctx,
            ALICE,
            ASSET,
            IFixture::mintCall {
                account: PAYNOTE_ADDRESS,
                amount: U256::from(DEPOSIT_AMOUNT),
            },
        );
        configure_asset(&mut ctx, mode);
        let before = asset_balances(&mut ctx);

        let result = run_call!(
            &mut ctx,
            PAYNOTE_ADDRESS,
            deposit_calldata(ASSET, DEPOSIT_AMOUNT),
            false
        );
        assert!(
            !matches!(result.status, SubCallStatus::Success),
            "mode {mode} must not deposit"
        );
        assert!(
            String::from_utf8_lossy(&result.returndata).contains(reason),
            "mode {mode}: 0x{}",
            alloy_primitives::hex::encode(&result.returndata)
        );
        assert_eq!(asset_balances(&mut ctx), before, "mode {mode}");
        assert_pristine!(&mut ctx);
    }
}

#[test]
fn a_router_pull_that_leaves_tokens_in_the_pool_deposits_nothing() {
    // Mode 10: the router's pull debits the pool one unit short while the vault
    // still receives the full amount.
    let mut ctx = evm_ctx(seeded_db(true, true));
    configure_asset(&mut ctx, 10);
    let before = asset_balances(&mut ctx);

    let result = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        deposit_calldata(ASSET, DEPOSIT_AMOUNT),
        false
    );
    assert!(
        !matches!(result.status, SubCallStatus::Success),
        "a pool left off its starting balance must not deposit"
    );
    assert!(
        String::from_utf8_lossy(&result.returndata)
            .contains("PayNote token moved an unexpected amount"),
        "0x{}",
        alloy_primitives::hex::encode(&result.returndata)
    );
    assert_eq!(asset_balances(&mut ctx), before);
    assert_pristine!(&mut ctx);
}

#[test]
fn a_token_that_returns_nothing_still_deposits() {
    let mut ctx = evm_ctx(seeded_db(true, true));
    configure_asset(&mut ctx, 7);
    let [alice, ..] = asset_balances(&mut ctx);

    let result = run_call!(
        &mut ctx,
        PAYNOTE_ADDRESS,
        deposit_calldata(ASSET, DEPOSIT_AMOUNT),
        false
    );
    assert!(
        matches!(result.status, SubCallStatus::Success),
        "{:?}",
        result.status
    );
    let amount = U256::from(DEPOSIT_AMOUNT);
    assert_eq!(
        asset_balances(&mut ctx),
        [alice - amount, U256::ZERO, U256::ZERO, amount]
    );
}
