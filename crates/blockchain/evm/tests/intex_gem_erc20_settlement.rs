//! Real Intex/Gem factory and router execution with stateful ERC20, vault and NFT counterparties.
use std::sync::Arc;

use alloy_primitives::{Address, Bytes, FixedBytes, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use outbe_compressed_entities::ExecutionScope;
use outbe_evm::sub_call;
use outbe_gem::{GemAddParams, GemState};
use outbe_gemfactory::precompile::IGemFactory;
use outbe_intex::{CreateSeriesParams, IntexCallTrigger, SeriesId};
use outbe_intexfactory::precompile::IIntexFactory;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::{
    addresses::{
        GEM_FACTORY_ADDRESS, INTEX_FACTORY_ADDRESS, INTEX_NFT1155_ADDRESS, VAULT_ROUTER_ADDRESS,
    },
    block::BlockContext,
    chain::CHAIN_ID,
    storage::{
        direct::DirectStorageProvider, StorageHandle, SubCallInput, SubCallOutput, SubCallStatus,
    },
    time::WorldwideDay,
};
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
        function mint1155(address account, uint256 id, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
        function configure(uint256 mode, address factory, bytes reentry) external;
        function callbackRejected() external view returns (bool);
    }
}

const OWNER: Address = Address::new([0x11; 20]);
const ASSET: Address = Address::new([0x33; 20]);
const VAULT: Address = Address::new([0x55; 20]);
const TIMESTAMP: u64 = 1_700_000_000;
const SERIES: [u8; 14] = *b"20241220-USD-U";
const UNITS: u64 = 2;
type EvmCtx = revm::Context<
    revm::context::BlockEnv,
    revm::context::TxEnv,
    revm::context::CfgEnv,
    CacheDB<EmptyDB>,
>;

#[derive(Clone, Copy, Debug)]
enum Factory {
    Intex,
    Gem,
}

impl Factory {
    fn address(self) -> Address {
        match self {
            Factory::Intex => INTEX_FACTORY_ADDRESS,
            Factory::Gem => GEM_FACTORY_ADDRESS,
        }
    }

    fn source(self) -> IVaultRouter::StablesSource {
        match self {
            Factory::Intex => IVaultRouter::StablesSource::IntexCostAmount,
            Factory::Gem => IVaultRouter::StablesSource::GemCostAmount,
        }
    }

    fn settle_call(self, gem_id: U256) -> Bytes {
        match self {
            Factory::Intex => IIntexFactory::settleIntexCall {
                seriesId: FixedBytes(SERIES),
                intexOwner: OWNER,
                amount: U256::from(UNITS),
                asset: ASSET,
            }
            .abi_encode(),
            Factory::Gem => IGemFactory::settleGemCall {
                gemId: gem_id,
                asset: ASSET,
            }
            .abi_encode(),
        }
        .into()
    }
}

struct World {
    ctx: EvmCtx,
    scope: Arc<ExecutionScope>,
    readers: RuntimeBodyReaders,
    factory: Factory,
    payer: Address,
    cost: U256,
    gem_id: U256,
}

impl World {
    fn new(factory: Factory, payer: Address, registered: bool) -> Self {
        let mut db = CacheDB::new(EmptyDB::default());
        let code = Bytecode::new_raw(Bytes::from(
            alloy_primitives::hex::decode(include_str!("fixtures/FactorySettlement.hex").trim())
                .unwrap(),
        ));
        for addr in [ASSET, VAULT, INTEX_NFT1155_ADDRESS] {
            db.insert_account_info(
                addr,
                AccountInfo {
                    code_hash: code.hash_slow(),
                    code: Some(code.clone()),
                    ..Default::default()
                },
            );
        }
        let readers = RuntimeBodyReaders::new(Arc::new(MemoryStorage::new()));
        let scope = Arc::new(ExecutionScope::new());
        let block = BlockContext::new(1, TIMESTAMP, CHAIN_ID, OWNER, vec![OWNER]);
        let mut provider = DirectStorageProvider::new(&mut db, block);
        let gem_id = StorageHandle::enter(&mut provider, |storage| {
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
                router.liquidity_sources.insert(factory.address()).unwrap();
                router
                    .liquidity_source_types
                    .write(&factory.address(), factory.source() as u8)
                    .unwrap();
            }
            // A finalized day above the floor qualifies the series or gem.
            let day = outbe_primitives::time::first_full_day(TIMESTAMP);
            let pair = outbe_oracle::api::register_pair(
                storage.clone(),
                outbe_oracle::api::AddressPair::new_coen_to(840),
            )
            .unwrap();
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            oracle
                .utc_day_vwap_value
                .get_nested(&day)
                .write(&pair, U256::from(2_160_001))
                .unwrap();
            oracle.utc_day_vwap_last_finalized.write(day).unwrap();
            match factory {
                Factory::Intex => {
                    let series_id = SeriesId::from(FixedBytes(SERIES));
                    outbe_intex::api::create_series(
                        &storage,
                        CreateSeriesParams {
                            series_id,
                            worldwide_day: WorldwideDay::new(20_241_220),
                            issued_units: 10,
                            promis_load_minor: 1_500_000,
                            entry_price_minor: U256::from(2_000_000),
                            floor_price_minor: U256::from(2_160_000),
                            call_price_minor: U256::from(4_560_000),
                            call_trigger: IntexCallTrigger {
                                call_window_seconds: 1,
                                call_threshold_seconds: 1,
                                call_notice_period_seconds: 1,
                            },
                            issued_at: TIMESTAMP as u32,
                            issuance_currency: 840,
                            reference_currency: 840,
                        },
                    )
                    .unwrap();
                    U256::ZERO
                }
                Factory::Gem => outbe_gem::api::add_gem(
                    &storage,
                    GemAddParams {
                        owner: OWNER,
                        gem_type: outbe_gemfactory::schema::GemTypes::Wallet as u8,
                        promis_load_minor: U256::from(1_500_000),
                        entry_price_minor: U256::from(2_000_000),
                        floor_price_minor: U256::from(2_160_000),
                        call_price_minor: U256::from(4_560_000),
                        call_rate: 128,
                        issuance_currency: 840,
                        reference_currency: 840,
                        initial_state: GemState::Issued,
                        issued_at: TIMESTAMP,
                    },
                )
                .unwrap(),
            }
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
            factory,
            payer,
            cost: U256::ZERO,
            gem_id,
        };
        world.cost = world.quote();
        assert!(!world.cost.is_zero());
        world.ok(
            OWNER,
            INTEX_NFT1155_ADDRESS,
            IFixture::mint1155Call {
                account: OWNER,
                id: U256::from_be_slice(&SERIES),
                amount: U256::from(UNITS),
            },
        );
        world.ok(
            OWNER,
            ASSET,
            IFixture::mintCall {
                account: payer,
                amount: world.cost * U256::from(2),
            },
        );
        // Existing factory funds must never subsidize a failed or partial payment.
        world.ok(
            OWNER,
            ASSET,
            IFixture::mintCall {
                account: factory.address(),
                amount: U256::from(17),
            },
        );
        world.ok(
            payer,
            ASSET,
            IFixture::approveCall {
                spender: factory.address(),
                amount: world.cost,
            },
        );
        world.ok(
            VAULT_ROUTER_ADDRESS,
            ASSET,
            IFixture::approveCall {
                spender: VAULT,
                amount: U256::MAX,
            },
        );
        world
    }

    fn call(
        &mut self,
        caller: Address,
        target: Address,
        calldata: Bytes,
        is_static: bool,
    ) -> SubCallOutput {
        sub_call::run(
            &mut self.ctx,
            caller,
            false,
            SpecId::PRAGUE,
            Some(self.readers.clone()),
            self.scope.clone(),
            SubCallInput {
                target,
                value: U256::ZERO,
                calldata,
                gas_limit: 5_000_000,
                is_static,
            },
        )
        .unwrap()
    }

    fn ok<C: SolCall>(&mut self, caller: Address, target: Address, call: C) -> C::Return {
        let out = self.call(caller, target, call.abi_encode().into(), false);
        assert!(
            matches!(out.status, SubCallStatus::Success),
            "{:?}",
            out.status
        );
        C::abi_decode_returns(&out.returndata).unwrap()
    }

    fn view<C: SolCall>(&mut self, target: Address, call: C) -> C::Return {
        let out = self.call(OWNER, target, call.abi_encode().into(), true);
        assert!(
            matches!(out.status, SubCallStatus::Success),
            "{:?}",
            out.status
        );
        C::abi_decode_returns(&out.returndata).unwrap()
    }

    fn quote(&mut self) -> U256 {
        match self.factory {
            Factory::Intex => {
                self.view(
                    INTEX_FACTORY_ADDRESS,
                    IIntexFactory::quoteSettlementCall {
                        seriesId: FixedBytes(SERIES),
                        paymentToken: ASSET,
                        amount: U256::from(UNITS),
                    },
                )
                .payableUnits
            }
            Factory::Gem => {
                self.view(
                    GEM_FACTORY_ADDRESS,
                    IGemFactory::quoteSettlementCall {
                        gemId: self.gem_id,
                        asset: ASSET,
                    },
                )
                .payableUnits
            }
        }
    }

    fn settle(&mut self) -> SubCallOutput {
        let (payer, target, calldata) = (
            self.payer,
            self.factory.address(),
            self.factory.settle_call(self.gem_id),
        );
        self.call(payer, target, calldata, false)
    }

    fn configure(&mut self, target: Address, mode: u64) {
        let (factory, reentry) = (
            self.factory.address(),
            self.factory.settle_call(self.gem_id),
        );
        self.ok(
            OWNER,
            target,
            IFixture::configureCall {
                mode: U256::from(mode),
                factory,
                reentry,
            },
        );
    }

    fn balances(&mut self) -> [U256; 5] {
        let [payer, factory, router, vault] = [
            self.payer,
            self.factory.address(),
            VAULT_ROUTER_ADDRESS,
            VAULT,
        ]
        .map(|account| self.view(ASSET, IFixture::balanceOfCall { account }));
        let shares = self.view(
            VAULT,
            IFixture::balanceOfCall {
                account: VAULT_ROUTER_ADDRESS,
            },
        );
        [payer, factory, router, vault, shares]
    }

    fn allowances(&mut self) -> [U256; 3] {
        [
            (self.payer, self.factory.address()),
            (self.factory.address(), VAULT_ROUTER_ADDRESS),
            (VAULT_ROUTER_ADDRESS, VAULT),
        ]
        .map(|(owner, spender)| self.view(ASSET, IFixture::allowanceCall { owner, spender }))
    }

    fn paid_balances(&self) -> [U256; 5] {
        [self.cost, U256::from(17), U256::ZERO, self.cost, self.cost]
    }

    fn assert_settled(&self) {
        let logs = self.ctx.journaled_state.logs();
        match self.factory {
            Factory::Intex => {
                let settled: Vec<_> = logs
                    .iter()
                    .filter_map(|log| IIntexFactory::Settled::decode_log_data(&log.data).ok())
                    .collect();
                assert_eq!(settled.len(), 1);
                assert_eq!(settled[0].intexOwner, OWNER);
                assert_eq!(settled[0].amount, U256::from(UNITS));
            }
            Factory::Gem => {
                let settled: Vec<_> = logs
                    .iter()
                    .filter_map(|log| IGemFactory::GemSettled::decode_log_data(&log.data).ok())
                    .collect();
                assert_eq!(settled.len(), 1);
                assert_eq!(settled[0].owner, OWNER);
                assert_eq!(settled[0].amountPaid, self.cost);
            }
        }
    }

    fn settled_intex_units(&mut self) -> U256 {
        sol! { function balanceOf(address account, uint256 id) external view returns (uint256); }
        self.view(
            INTEX_NFT1155_ADDRESS,
            balanceOfCall {
                account: OWNER,
                id: U256::from_be_slice(&SERIES) | (U256::ONE << 112),
            },
        )
    }
}

const FACTORIES: [Factory; 2] = [Factory::Intex, Factory::Gem];

#[test]
fn erc20_settlement_moves_exactly_the_quoted_cost_into_the_reserve() {
    for factory in FACTORIES {
        let mut world = World::new(factory, OWNER, true);
        assert!(
            matches!(world.settle().status, SubCallStatus::Success),
            "{factory:?}"
        );
        assert_eq!(world.balances(), world.paid_balances(), "{factory:?}");
        world.assert_settled();

        let balances = world.balances();
        assert!(!matches!(world.settle().status, SubCallStatus::Success));
        assert_eq!(world.balances(), balances, "{factory:?}");
    }
}

#[test]
fn a_third_party_pays_and_the_units_stay_with_the_owner() {
    let payer = Address::new([0x77; 20]);
    let mut world = World::new(Factory::Intex, payer, true);
    assert!(matches!(world.settle().status, SubCallStatus::Success));
    assert_eq!(world.balances(), world.paid_balances());
    assert_eq!(world.settled_intex_units(), U256::from(UNITS));
}

#[test]
fn payment_failures_restore_balances_allowances_and_logs() {
    // False transfer/approve/router transfer, vault revert, malformed bool,
    // success without movement, fee-on-transfer, and missing router authorization.
    for factory in FACTORIES {
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
            let mut world = World::new(factory, OWNER, registered);
            world.configure(target, mode);
            let balances = world.balances();
            let allowances = world.allowances();
            let logs = world.ctx.journaled_state.logs().to_vec();
            assert!(
                !matches!(world.settle().status, SubCallStatus::Success),
                "{factory:?} mode {mode}"
            );
            assert_eq!(world.balances(), balances, "{factory:?} mode {mode}");
            assert_eq!(world.allowances(), allowances, "{factory:?} mode {mode}");
            assert_eq!(
                world.ctx.journaled_state.logs(),
                logs,
                "{factory:?} mode {mode}"
            );

            // The rollback left the entity settleable.
            world.configure(target, 0);
            if registered {
                assert!(
                    matches!(world.settle().status, SubCallStatus::Success),
                    "{factory:?} mode {mode}"
                );
            }
        }
    }
}

#[test]
fn optional_empty_token_returns_are_accepted() {
    for factory in FACTORIES {
        let mut world = World::new(factory, OWNER, true);
        world.configure(ASSET, 7);
        assert!(matches!(world.settle().status, SubCallStatus::Success));
        assert_eq!(world.balances(), world.paid_balances(), "{factory:?}");
    }
}

#[test]
fn the_payer_cannot_reenter_settlement_during_transfer() {
    for factory in FACTORIES {
        let mut world = World::new(factory, ASSET, true);
        world.configure(ASSET, 5);
        assert!(matches!(world.settle().status, SubCallStatus::Success));
        assert!(world.view(ASSET, IFixture::callbackRejectedCall {}));
        assert_eq!(world.balances(), world.paid_balances(), "{factory:?}");
    }
}
