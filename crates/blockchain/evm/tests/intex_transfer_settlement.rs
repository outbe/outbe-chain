//! Issued Intex units change hands on the real `IntexNFT1155` bytecode (`fixtures/*.hex`: forge
//! `deployedBytecode`, `IntexMetadata` linked at [`METADATA_LIB`]) and settle for their holder.
use outbe_offchain_data::runtime_body_readers;
use std::sync::Arc;

use alloy_primitives::{keccak256, Address, Bytes, FixedBytes, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use outbe_compressed_entities::ExecutionScope;
use outbe_intex::{CreateSeriesParams, IntexCallTrigger, SeriesId};
use outbe_intexfactory::precompile::IIntexFactory;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::{
    addresses::{INTEX_FACTORY_ADDRESS, INTEX_NFT1155_ADDRESS, VAULT_ROUTER_ADDRESS},
    block::BlockContext,
    chain::CHAIN_ID,
    storage::{direct::DirectStorageProvider, StorageHandle, SubCallOutput, SubCallStatus},
    time::WorldwideDay,
};
use outbe_vaultrouter::{api::IVaultRouter, VaultRouterContract};
use revm::{
    context_interface::JournalTr,
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    state::{AccountInfo, Bytecode},
    Context,
};

#[path = "support/intex_sub_call.rs"]
mod intex_sub_call;

sol!("../../../contracts/intex/src/shared/interfaces/IIntexNFT1155.sol");

sol! {
    interface IErc20Fixture {
        function mint(address account, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }

    interface INft {
        function initialize(address defaultAdmin) external;
        function grantRole(bytes32 role, address account) external;
        function balanceOf(address account, uint256 id) external view returns (uint256);
        function totalSupply(uint256 tokenId) external view returns (uint256);
        function safeTransferFrom(address from, address to, uint256 id, uint256 value, bytes data) external;
    }
}

const ALICE: Address = Address::new([0x11; 20]);
const BOB: Address = Address::new([0x22; 20]);
const PAYER: Address = Address::new([0x77; 20]);
const ADMIN: Address = Address::new([0xad; 20]);
const RELAYER: Address = Address::new([0x4e; 20]);
const ASSET: Address = Address::new([0x33; 20]);
const VAULT: Address = Address::new([0x55; 20]);
/// Where the linked `IntexMetadata` library lives. `fixtures/IntexNFT1155.hex` delegates there.
const METADATA_LIB: Address = Address::new([0x4c; 20]);
const TIMESTAMP: u64 = 1_700_000_000;
const SERIES: [u8; 14] = *b"20241220-USD-U";
const ISSUED_UNITS: u64 = 10;
const TRANSFERRED_UNITS: u64 = 4;
const SETTLED_UNITS: u64 = 3;

type EvmCtx = revm::Context<
    revm::context::BlockEnv,
    revm::context::TxEnv,
    revm::context::CfgEnv,
    CacheDB<EmptyDB>,
>;

fn issued_id() -> U256 {
    U256::from_be_slice(&SERIES)
}

fn settled_id() -> U256 {
    issued_id() | (U256::ONE << 112)
}

fn runtime_code(hex: &str) -> Bytecode {
    Bytecode::new_raw(Bytes::from(
        alloy_primitives::hex::decode(hex.trim()).unwrap(),
    ))
}

struct World {
    ctx: EvmCtx,
    scope: Arc<ExecutionScope>,
    readers: RuntimeBodyReaders,
    cost: U256,
}

impl World {
    /// Alice holds ten issued units on the real NFT. The Rust series record
    /// mirrors the contract, and a finalized day above the floor qualifies it.
    fn new() -> Self {
        let mut db = CacheDB::new(EmptyDB::default());
        let counterparty = runtime_code(include_str!("fixtures/FactorySettlement.hex"));
        for addr in [ASSET, VAULT] {
            db.insert_account_info(
                addr,
                AccountInfo {
                    code_hash: counterparty.hash_slow(),
                    code: Some(counterparty.clone()),
                    ..Default::default()
                },
            );
        }
        for (addr, hex) in [
            (
                INTEX_NFT1155_ADDRESS,
                include_str!("fixtures/IntexNFT1155.hex"),
            ),
            (METADATA_LIB, include_str!("fixtures/IntexMetadata.hex")),
        ] {
            let code = runtime_code(hex);
            db.insert_account_info(
                addr,
                AccountInfo {
                    code_hash: code.hash_slow(),
                    code: Some(code),
                    ..Default::default()
                },
            );
        }
        let readers = runtime_body_readers(Arc::new(MemoryStorage::new()));
        let scope = Arc::new(ExecutionScope::default());
        let block = BlockContext::new(1, TIMESTAMP, CHAIN_ID, ALICE, vec![ALICE]);
        let mut provider = DirectStorageProvider::new(&mut db, block);
        StorageHandle::enter(&mut provider, |storage| {
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
            router
                .liquidity_sources
                .insert(INTEX_FACTORY_ADDRESS)
                .unwrap();
            router
                .liquidity_source_types
                .write(
                    &INTEX_FACTORY_ADDRESS,
                    IVaultRouter::StablesSource::IntexCostAmount as u8,
                )
                .unwrap();
            let day = outbe_primitives::time::first_full_day(TIMESTAMP);
            let pair = outbe_oracle::api::register_pair(
                storage.clone(),
                outbe_oracle::api::AddressPair::new_coen_to(840),
            )
            .unwrap();
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            oracle
                .record_utc_day_vwap(day, pair, U256::from(2_160_001))
                .unwrap();
            oracle.utc_day_vwap_last_finalized.write(day).unwrap();
            outbe_intex::api::create_series(
                &storage,
                CreateSeriesParams {
                    series_id: SeriesId::from(FixedBytes(SERIES)),
                    worldwide_day: WorldwideDay::new(20_241_220),
                    issued_units: ISSUED_UNITS as u32,
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
            cost: U256::ZERO,
        };
        world.install_nft();
        world.cost = world.quote(SETTLED_UNITS);
        assert!(!world.cost.is_zero());
        world.ok(
            ALICE,
            ASSET,
            IErc20Fixture::mintCall {
                account: PAYER,
                amount: world.cost * U256::from(4),
            },
        );
        world.ok(
            PAYER,
            ASSET,
            IErc20Fixture::approveCall {
                spender: INTEX_FACTORY_ADDRESS,
                amount: U256::MAX,
            },
        );
        world.ok(
            VAULT_ROUTER_ADDRESS,
            ASSET,
            IErc20Fixture::approveCall {
                spender: VAULT,
                amount: U256::MAX,
            },
        );
        world
    }

    /// Initialized in place: the admin grants the relayer role to the issuer and the
    /// settlement role to the factory precompile.
    fn install_nft(&mut self) {
        self.ok(
            ADMIN,
            INTEX_NFT1155_ADDRESS,
            INft::initializeCall {
                defaultAdmin: ADMIN,
            },
        );
        for (role, account) in [
            ("RELAYER_ROLE", RELAYER),
            ("SETTLEMENT_ROLE", INTEX_FACTORY_ADDRESS),
        ] {
            self.ok(
                ADMIN,
                INTEX_NFT1155_ADDRESS,
                INft::grantRoleCall {
                    role: keccak256(role.as_bytes()),
                    account,
                },
            );
        }
        self.ok(
            RELAYER,
            INTEX_NFT1155_ADDRESS,
            IIntexNFT1155::createSeriesCall {
                params: IIntexNFT1155::CreateSeriesParams {
                    seriesId: FixedBytes(SERIES),
                    worldwideDay: 20_241_220,
                    issuedAt: TIMESTAMP as u32,
                    issuanceCurrency: 840,
                    referenceCurrency: 840,
                    issuedUnits: ISSUED_UNITS as u32,
                    promisLoadMinor: 1_500_000,
                    entryPriceMinor: 2_000_000,
                    floorPriceMinor: 2_160_000,
                    callPriceMinor: 4_560_000,
                    callTrigger: IIntexNFT1155::IntexCallTrigger {
                        callWindow: 1,
                        callThreshold: 1,
                        callNoticePeriod: 1,
                    },
                },
            },
        );
        self.ok(
            RELAYER,
            INTEX_NFT1155_ADDRESS,
            IIntexNFT1155::issueIntexCall {
                to: ALICE,
                units: U256::from(ISSUED_UNITS),
                seriesId: FixedBytes(SERIES),
            },
        );
    }

    fn call(
        &mut self,
        caller: Address,
        target: Address,
        calldata: Bytes,
        is_static: bool,
    ) -> SubCallOutput {
        intex_sub_call::IntexSubCall {
            ctx: &mut self.ctx,
            scope: &self.scope,
            readers: &self.readers,
        }
        .call(caller, target, calldata, is_static)
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
        let out = self.call(ALICE, target, call.abi_encode().into(), true);
        assert!(
            matches!(out.status, SubCallStatus::Success),
            "{:?}",
            out.status
        );
        C::abi_decode_returns(&out.returndata).unwrap()
    }

    fn quote(&mut self, units: u64) -> U256 {
        self.view(
            INTEX_FACTORY_ADDRESS,
            IIntexFactory::quoteSettlementCall {
                seriesId: FixedBytes(SERIES),
                asset: ASSET,
                units: U256::from(units),
            },
        )
        .paymentMinor
    }

    /// Alice moves part of her issued holding to Bob with a plain ERC-1155 transfer.
    fn transfer_to_bob(&mut self, units: u64) {
        self.ok(
            ALICE,
            INTEX_NFT1155_ADDRESS,
            INft::safeTransferFromCall {
                from: ALICE,
                to: BOB,
                id: issued_id(),
                value: U256::from(units),
                data: Bytes::new(),
            },
        );
    }

    /// The payer, a third party, settles `units` for `owner` through the factory precompile.
    fn settle_for(&mut self, owner: Address, units: u64) -> SubCallOutput {
        let calldata = IIntexFactory::settleIntexCall {
            seriesId: FixedBytes(SERIES),
            owner,
            units: U256::from(units),
            asset: ASSET,
            snapshotId: U256::ZERO,
        }
        .abi_encode();
        self.call(PAYER, INTEX_FACTORY_ADDRESS, calldata.into(), false)
    }

    fn nft_balance(&mut self, account: Address, id: U256) -> u64 {
        self.view(INTEX_NFT1155_ADDRESS, INft::balanceOfCall { account, id })
            .to::<u64>()
    }

    /// `[issued Alice, issued Bob, settled Alice, settled Bob]`.
    fn holdings(&mut self) -> [u64; 4] {
        [
            self.nft_balance(ALICE, issued_id()),
            self.nft_balance(BOB, issued_id()),
            self.nft_balance(ALICE, settled_id()),
            self.nft_balance(BOB, settled_id()),
        ]
    }

    fn supplies(&mut self) -> [u64; 2] {
        [issued_id(), settled_id()].map(|token_id| {
            self.view(
                INTEX_NFT1155_ADDRESS,
                INft::totalSupplyCall { tokenId: token_id },
            )
            .to::<u64>()
        })
    }

    fn asset_balance(&mut self, account: Address) -> U256 {
        self.view(ASSET, IErc20Fixture::balanceOfCall { account })
    }

    fn settled_events(&self) -> Vec<(Address, u64)> {
        self.ctx
            .journaled_state
            .logs()
            .iter()
            .filter_map(|log| IIntexFactory::Settled::decode_log_data(&log.data).ok())
            .map(|event| (event.owner, event.units.to::<u64>()))
            .collect()
    }
}

#[test]
fn transferred_unsettled_intex_units_settle_to_their_new_owner_and_never_to_the_sender() {
    let mut world = World::new();
    assert_eq!(world.holdings(), [ISSUED_UNITS, 0, 0, 0]);

    world.transfer_to_bob(TRANSFERRED_UNITS);
    assert_eq!(
        world.holdings(),
        [6, 4, 0, 0],
        "a real ERC-1155 transfer moved four units"
    );

    let payer_before = world.asset_balance(PAYER);
    let vault_before = world.asset_balance(VAULT);
    let out = world.settle_for(BOB, SETTLED_UNITS);
    assert!(
        matches!(out.status, SubCallStatus::Success),
        "{:?}",
        out.status
    );

    assert_eq!(
        world.holdings(),
        [6, 1, 0, 3],
        "the new owner's units settle; the sender's six are untouched"
    );
    assert_eq!(
        world.supplies(),
        [7, 3],
        "issued and settled supplies move together"
    );
    assert_eq!(world.settled_events(), vec![(BOB, SETTLED_UNITS)]);
    assert_eq!(world.asset_balance(PAYER), payer_before - world.cost);
    assert_eq!(world.asset_balance(VAULT), vault_before + world.cost);

    // Neither party can reach across the transfer: Bob holds one unit and Alice six.
    let holdings = world.holdings();
    let supplies = world.supplies();
    let logs = world.ctx.journaled_state.logs().to_vec();
    let payer_after = world.asset_balance(PAYER);
    for (owner, units) in [(BOB, 2), (ALICE, 7), (ALICE, ISSUED_UNITS)] {
        let out = world.settle_for(owner, units);
        assert!(
            matches!(out.status, SubCallStatus::Revert(_)),
            "{owner:?} x{units}: {:?}",
            out.status
        );
        assert_eq!(world.holdings(), holdings, "{owner:?} x{units}");
        assert_eq!(world.supplies(), supplies, "{owner:?} x{units}");
        assert_eq!(world.ctx.journaled_state.logs(), logs, "{owner:?} x{units}");
        assert_eq!(
            world.asset_balance(PAYER),
            payer_after,
            "{owner:?} x{units}"
        );
    }

    // Each side still settles exactly what it holds.
    let out = world.settle_for(BOB, 1);
    assert!(
        matches!(out.status, SubCallStatus::Success),
        "{:?}",
        out.status
    );
    let out = world.settle_for(ALICE, 6);
    assert!(
        matches!(out.status, SubCallStatus::Success),
        "{:?}",
        out.status
    );
    assert_eq!(world.holdings(), [0, 0, 6, 4]);
    assert_eq!(world.supplies(), [0, ISSUED_UNITS]);
}
