//! Real NOD/router execution with stateful ERC20 and reserve-vault counterparties.
use outbe_offchain_data::runtime_body_readers;
#[path = "common/nod_qualification.rs"]
mod qualify_fixture;
use qualify_fixture::qualify;

use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use outbe_compressed_entities::{begin_block, ExecutionScope, WwdEntityId};
use outbe_evm::sub_call;
use outbe_gratis::enclave_client::test_enclave;
use outbe_nod::{precompile::INod, NodIssueParams};
use outbe_nodfactory::precompile::INodFactory;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::{
    addresses::{
        COMPRESSED_ENTITIES_ADDRESS, NOD_ADDRESS, NOD_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS,
    },
    block::BlockContext,
    chain::CHAIN_ID,
    storage::{
        direct::DirectStorageProvider, StorageHandle, SubCallInput, SubCallOutput, SubCallStatus,
    },
    time::WorldwideDay,
};
use outbe_tee::protocol::GratisOp;
use outbe_tee_enclave::gratis::{derive_modify_key, modify_mac};
use outbe_vaultrouter::{api::IVaultRouter, VaultRouterContract};
use revm::{
    context_interface::JournalTr,
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
        function allowance(address owner, address spender) external view returns (uint256);
        function configure(uint256 mode, uint256 nodId) external;
        function callbackRejected() external view returns (bool);
    }
}

const OWNER: Address = Address::new([0x11; 20]);
const ASSET: Address = Address::new([0x33; 20]);
const VAULT: Address = Address::new([0x55; 20]);
const GRATIS_LOAD: u64 = 1_000;
const TIMESTAMP: u64 = 1_700_000_000;

type EvmCtx = revm::Context<
    revm::context::BlockEnv,
    revm::context::TxEnv,
    revm::context::CfgEnv,
    CacheDB<EmptyDB>,
>;

struct World {
    ctx: EvmCtx,
    scope: Arc<ExecutionScope>,
    readers: RuntimeBodyReaders,
    owner: Address,
    nod: WwdEntityId,
    cost: U256,
}

impl World {
    fn new(owner: Address, cost: U256, registered: bool) -> Self {
        test_enclave::install();
        let mut db = CacheDB::new(EmptyDB::default());
        let code = Bytecode::new_raw(Bytes::from(
            alloy_primitives::hex::decode(include_str!("fixtures/NodSettlement.hex").trim())
                .unwrap(),
        ));
        for addr in [ASSET, VAULT] {
            db.insert_account_info(
                addr,
                AccountInfo {
                    code_hash: code.hash_slow(),
                    code: Some(code.clone()),
                    ..Default::default()
                },
            );
        }
        let adapter = Arc::new(MemoryStorage::new());
        let readers = runtime_body_readers(adapter.clone());
        let parent = outbe_nod::nod_reader(adapter);
        let scope = Arc::new(ExecutionScope::default());
        let block = BlockContext::new(1, TIMESTAMP, CHAIN_ID, OWNER, vec![OWNER]);
        let mut provider = DirectStorageProvider::new(&mut db, block);
        let nod = StorageHandle::enter(&mut provider, |storage| {
            seed_settlement_nod(
                &storage,
                &scope,
                &parent,
                NodSettlementSeed {
                    owner,
                    cost,
                    registered,
                },
            )
        });
        provider.flush().unwrap();
        let ctx = Context::mainnet()
            .with_db(db)
            .modify_cfg_chained(|cfg| cfg.chain_id = CHAIN_ID)
            .modify_block_chained(|block| block.timestamp = U256::from(TIMESTAMP));
        let mut world = Self {
            ctx,
            scope,
            readers,
            owner,
            nod,
            cost,
        };
        world.fund_settlement();
        world
    }

    fn fund_settlement(&mut self) {
        let owner = self.owner;
        let nod = self.nod;
        let cost = self.cost;
        let quote = self.view(
            NOD_FACTORY_ADDRESS,
            INodFactory::quoteSettlementCall {
                nodId: nod.to_u256(),
                asset: ASSET,
            },
        );
        assert_eq!(quote.settlementCurrency, 840);
        assert_eq!(quote.paymentMinor, cost);
        assert_eq!(quote.snapshotId, U256::ZERO);
        self.ok(
            owner,
            ASSET,
            IFixture::mintCall {
                account: owner,
                amount: cost * U256::from(2),
            },
        );
        // Existing factory funds must never subsidize a failed or partial payment.
        self.ok(
            owner,
            ASSET,
            IFixture::mintCall {
                account: NOD_FACTORY_ADDRESS,
                amount: U256::from(17),
            },
        );
        self.ok(
            owner,
            ASSET,
            IFixture::approveCall {
                spender: NOD_FACTORY_ADDRESS,
                amount: cost,
            },
        );
        self.ok(
            VAULT_ROUTER_ADDRESS,
            ASSET,
            IFixture::approveCall {
                spender: VAULT,
                amount: U256::MAX,
            },
        );
    }

    fn call(
        &mut self,
        caller: Address,
        target: Address,
        call: impl SolCall,
        is_static: bool,
    ) -> SubCallOutput {
        sub_call::run(
            &mut self.ctx,
            sub_call::SubCallEnvironment {
                self_address: caller,
                outer_is_static: false,
                spec: SpecId::PRAGUE,
                runtime_body_readers: Some(self.readers.clone()),
                execution_scope: self.scope.clone(),
            },
            SubCallInput {
                target,
                value: U256::ZERO,
                calldata: call.abi_encode().into(),
                gas_limit: 5_000_000,
                is_static,
            },
        )
        .unwrap()
    }

    fn ok<C: SolCall>(&mut self, caller: Address, target: Address, call: C) -> C::Return {
        let out = self.call(caller, target, call, false);
        assert!(
            matches!(out.status, SubCallStatus::Success),
            "{:?}",
            out.status
        );
        C::abi_decode_returns(&out.returndata).unwrap()
    }

    fn view<C: SolCall>(&mut self, target: Address, call: C) -> C::Return {
        let out = self.call(self.owner, target, call, true);
        assert!(
            matches!(out.status, SubCallStatus::Success),
            "{:?}",
            out.status
        );
        C::abi_decode_returns(&out.returndata).unwrap()
    }

    fn settle(&mut self) -> SubCallOutput {
        self.call(
            self.owner,
            NOD_FACTORY_ADDRESS,
            INodFactory::settleNodCall {
                nodId: self.nod.to_u256(),
                asset: ASSET,
                snapshotId: U256::ZERO,
            },
            false,
        )
    }

    fn configure(&mut self, target: Address, mode: u64) {
        self.ok(
            self.owner,
            target,
            IFixture::configureCall {
                mode: U256::from(mode),
                nodId: self.nod.to_u256(),
            },
        );
    }

    fn balances(&mut self) -> [U256; 5] {
        let [owner, factory, router, vault] =
            [self.owner, NOD_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS, VAULT]
                .map(|account| self.view(ASSET, IFixture::balanceOfCall { account }));
        let shares = self.view(
            VAULT,
            IFixture::balanceOfCall {
                account: VAULT_ROUTER_ADDRESS,
            },
        );
        [owner, factory, router, vault, shares]
    }

    fn allowances(&mut self) -> [U256; 3] {
        [
            (self.owner, NOD_FACTORY_ADDRESS),
            (NOD_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS),
            (VAULT_ROUTER_ADDRESS, VAULT),
        ]
        .map(|(owner, spender)| self.view(ASSET, IFixture::allowanceCall { owner, spender }))
    }

    fn assert_unpaid(&mut self) {
        let data = self.view(
            NOD_ADDRESS,
            INod::nodDataCall {
                nodId: self.nod.to_u256(),
            },
        );
        assert!(!data.isSettled);
    }
}

#[test]
fn erc20_settlement_moves_exact_full_width_cost_and_preserves_mining() {
    // Above u128, yet inside the price ladder the call index bins by.
    let cost = (U256::ONE << 129) + U256::from(500);
    let mut world = World::new(OWNER, cost, true);
    assert!(matches!(world.settle().status, SubCallStatus::Success));
    assert_eq!(
        world.balances(),
        [cost, U256::from(17), U256::ZERO, cost, cost]
    );
    let paid: Vec<_> = world
        .ctx
        .journaled_state
        .logs()
        .iter()
        .filter_map(|log| INodFactory::NodPaid::decode_log_data(&log.data).ok())
        .collect();
    assert_eq!(paid.len(), 1);
    assert_eq!(paid[0].owner, OWNER);
    assert_eq!(paid[0].asset, ASSET);
    assert_eq!(paid[0].paymentMinor, cost);
    let balances = world.balances();
    assert!(!matches!(world.settle().status, SubCallStatus::Success));
    assert_eq!(world.balances(), balances);

    let nonce = (0..100_000)
        .find(|n| outbe_nodfactory::runtime::validate_pow(world.nod, world.owner, *n).is_ok())
        .unwrap();
    let key = derive_modify_key(&test_enclave::state_key(), OWNER).unwrap();
    let mac = modify_mac(
        &key,
        &outbe_tee_enclave::gratis::ModifyOperation {
            account: OWNER,
            op: GratisOp::Mint,
            amount: U256::from(GRATIS_LOAD),
            op_nonce: 0,
            chain_id: B256::from(U256::from(CHAIN_ID)),
        },
    );
    let minted = world.ok(
        OWNER,
        NOD_FACTORY_ADDRESS,
        INodFactory::mineGratisCall {
            nodId: world.nod.to_u256(),
            nonce,
            mac: B256::from(mac),
            opNonce: 0,
        },
    );
    let view_key =
        outbe_tee_enclave::gratis::derive_view_key(&test_enclave::state_key(), OWNER).unwrap();
    assert_eq!(
        outbe_tee::gratis_decrypt::decrypt_gratis_balance(&view_key, OWNER, &minted).unwrap(),
        U256::from(GRATIS_LOAD)
    );
}

#[test]
fn a_third_party_pays_and_the_nod_stays_with_the_owner() {
    let payer = Address::new([0x77; 20]);
    let mut world = World::new(OWNER, U256::from(500), true);
    let cost = world.cost;
    world.ok(
        OWNER,
        ASSET,
        IFixture::mintCall {
            account: payer,
            amount: cost,
        },
    );
    world.ok(
        payer,
        ASSET,
        IFixture::approveCall {
            spender: NOD_FACTORY_ADDRESS,
            amount: cost,
        },
    );
    let paid = world.call(
        payer,
        NOD_FACTORY_ADDRESS,
        INodFactory::settleNodCall {
            nodId: world.nod.to_u256(),
            asset: ASSET,
            snapshotId: U256::ZERO,
        },
        false,
    );
    assert!(matches!(paid.status, SubCallStatus::Success));

    assert_eq!(
        world.view(ASSET, IFixture::balanceOfCall { account: payer }),
        U256::ZERO
    );
    assert_eq!(
        world.balances(),
        [cost * U256::from(2), U256::from(17), U256::ZERO, cost, cost],
        "the owner's own funds are untouched"
    );
    let data = world.view(
        NOD_ADDRESS,
        INod::nodDataCall {
            nodId: world.nod.to_u256(),
        },
    );
    assert!(data.isSettled);
    assert_eq!(data.owner, OWNER);
    let events: Vec<_> = world
        .ctx
        .journaled_state
        .logs()
        .iter()
        .filter_map(|log| INodFactory::NodPaid::decode_log_data(&log.data).ok())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].owner, OWNER);
    assert_eq!(events[0].paymentMinor, cost);
}

#[test]
fn payment_failures_restore_balances_allowances_nod_and_logs() {
    // False transfer/approve/router transfer, vault revert, malformed bool,
    // success without movement, fee-on-transfer, and missing router authorization.
    for (target, mode, registered) in [
        (ASSET, 1, true),
        (ASSET, 2, true),
        (ASSET, 3, true),
        (VAULT, 4, true),
        (ASSET, 6, true),
        (ASSET, 8, true),
        (ASSET, 9, true),
        (ASSET, 0, false),
    ] {
        let mut world = World::new(OWNER, U256::from(500), registered);
        world.configure(target, mode);
        let balances = world.balances();
        let allowances = world.allowances();
        let logs = world.ctx.journaled_state.logs().to_vec();
        assert!(
            !matches!(world.settle().status, SubCallStatus::Success),
            "mode {mode}"
        );
        assert_eq!(world.balances(), balances, "mode {mode}");
        assert_eq!(world.allowances(), allowances, "mode {mode}");
        world.assert_unpaid();
        assert_eq!(world.ctx.journaled_state.logs(), logs);
    }
}

#[test]
fn insufficient_allowance_and_balance_revert_without_payment() {
    for (allowance, cost) in [(U256::ZERO, U256::from(500)), (U256::MAX, U256::from(500))] {
        let mut world = World::new(OWNER, cost, true);
        let payer = if allowance.is_zero() {
            OWNER
        } else {
            Address::new([0x77; 20])
        };
        if payer != OWNER {
            // Empty the owner through an authorized transfer before attempting settlement.
            sol! { function transferFrom(address from, address to, uint256 amount) external returns (bool); }
            world.ok(
                OWNER,
                ASSET,
                IFixture::approveCall {
                    spender: payer,
                    amount: cost * U256::from(2),
                },
            );
            world.ok(
                payer,
                ASSET,
                transferFromCall {
                    from: OWNER,
                    to: payer,
                    amount: cost * U256::from(2),
                },
            );
        }
        world.ok(
            OWNER,
            ASSET,
            IFixture::approveCall {
                spender: NOD_FACTORY_ADDRESS,
                amount: allowance,
            },
        );
        let balances = world.balances();
        assert!(!matches!(world.settle().status, SubCallStatus::Success));
        assert_eq!(world.balances(), balances);
        world.assert_unpaid();
    }
}

#[test]
fn optional_empty_token_returns_are_accepted() {
    let mut world = World::new(OWNER, U256::from(500), true);
    world.configure(ASSET, 7);
    assert!(matches!(world.settle().status, SubCallStatus::Success));
    assert_eq!(
        world.balances(),
        [
            world.cost,
            U256::from(17),
            U256::ZERO,
            world.cost,
            world.cost
        ]
    );
}

#[test]
fn token_owner_cannot_reenter_settlement_during_transfer() {
    let mut world = World::new(ASSET, U256::from(500), true);
    world.configure(ASSET, 5);
    assert!(matches!(world.settle().status, SubCallStatus::Success));
    assert!(world.view(ASSET, IFixture::callbackRejectedCall {}));
    assert_eq!(
        world.balances(),
        [
            world.cost,
            U256::from(17),
            U256::ZERO,
            world.cost,
            world.cost
        ]
    );
}

struct NodSettlementSeed {
    owner: Address,
    cost: U256,
    registered: bool,
}
fn seed_settlement_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl outbe_compressed_entities::ParentBodySource,
    seed: NodSettlementSeed,
) -> WwdEntityId {
    let NodSettlementSeed {
        owner,
        cost,
        registered,
    } = seed;
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
        .unwrap();
    storage
        .sstore(
            COMPRESSED_ENTITIES_ADDRESS,
            U256::ONE,
            U256::from_be_slice(
                outbe_compressed_entities::sealed_root(B256::ZERO)
                    .unwrap()
                    .as_slice(),
            ),
        )
        .unwrap();
    begin_block(storage.clone(), scope).unwrap();
    seed_settlement_router(storage, registered);
    let params = NodIssueParams {
        owner,
        gratis_load_minor: U256::from(GRATIS_LOAD),
        worldwide_day: WorldwideDay::new(20_241_220),
        league_id: 1,
        entry_price_minor: cost * U256::from(1_000),
        issuance_currency: 840,
        reference_currency: 840,
    };
    let nod = outbe_nodfactory::api::issue_nod(
        storage,
        scope,
        parent,
        &outbe_nod::test_support::encrypted_fixture(&params, CHAIN_ID),
    )
    .unwrap();
    let floor_price_minor =
        outbe_nod::pricing::floor_price_minor(params.entry_price_minor).unwrap();
    let bucket =
        outbe_nod::identity::bucket_key(params.worldwide_day, params.entry_price_minor, 840);
    qualify(storage, bucket, floor_price_minor, 840).expect("bucket qualifies");
    nod
}
fn seed_settlement_router(storage: &StorageHandle<'_>, registered: bool) {
    let router = VaultRouterContract::new(storage.clone());
    router.assets.insert(ASSET).unwrap();
    router.asset_vault_set(ASSET).insert(VAULT).unwrap();
    router
        .reference_currency_vault_set(840)
        .insert(VAULT)
        .unwrap();
    router
        .vault_reference_currencies
        .write(&VAULT, 840)
        .unwrap();
    if registered {
        router
            .liquidity_sources
            .insert(NOD_FACTORY_ADDRESS)
            .unwrap();
        router
            .liquidity_source_types
            .write(
                &NOD_FACTORY_ADDRESS,
                IVaultRouter::StablesSource::NodCostAmount as u8,
            )
            .unwrap();
    }
}
