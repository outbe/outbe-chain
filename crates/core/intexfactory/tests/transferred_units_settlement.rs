//! Issued Intex units change hands on the real `IntexNFT1155` bytecode (`fixtures/*.hex`: forge
//! `deployedBytecode`, `IntexMetadata` linked at [`METADATA_LIB`]) and settle for their holder.

use alloy_primitives::{keccak256, Address, Bytes, FixedBytes, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use outbe_evm::test_support::{install_code, runtime_code, seed_storage, EvmWorld};
use outbe_intex::precompile::IIntex;
use outbe_intex::{CreateSeriesParams, IntexCallTrigger, SeriesId};
use outbe_intexfactory::precompile::IIntexFactory;
use outbe_primitives::{
    addresses::{
        INTEX_ADDRESS, INTEX_FACTORY_ADDRESS, INTEX_NFT1155_ADDRESS, VAULT_ROUTER_ADDRESS,
    },
    storage::{SubCallOutput, SubCallStatus},
    time::WorldwideDay,
};
use outbe_vaultrouter::{
    api::IVaultRouter::StablesSource,
    test_support::{register_source, register_vault},
};
use revm::database::{CacheDB, EmptyDB};

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

fn issued_id() -> U256 {
    U256::from_be_slice(&SERIES)
}

struct World {
    evm: EvmWorld,
    cost: U256,
}

impl World {
    /// Alice holds ten issued units on the real NFT. The Rust series record
    /// mirrors the contract.
    fn new() -> Self {
        let mut db = CacheDB::new(EmptyDB::default());
        let counterparty = runtime_code(include_str!(
            "../../../../testing/fixtures/evm/FactorySettlement.hex"
        ));
        for addr in [ASSET, VAULT] {
            install_code(&mut db, addr, &counterparty);
        }
        for (addr, hex) in [
            (
                INTEX_NFT1155_ADDRESS,
                include_str!("fixtures/IntexNFT1155.hex"),
            ),
            (METADATA_LIB, include_str!("fixtures/IntexMetadata.hex")),
        ] {
            install_code(&mut db, addr, &runtime_code(hex));
        }
        seed_storage(&mut db, TIMESTAMP, ALICE, |storage| {
            register_vault(&storage, ASSET, VAULT, 840).unwrap();
            register_source(
                &storage,
                INTEX_FACTORY_ADDRESS,
                StablesSource::IntexCostAmount,
            )
            .unwrap();
            outbe_intex::api::create_series(
                &storage,
                CreateSeriesParams {
                    series_id: SeriesId::from(FixedBytes(SERIES)),
                    worldwide_day: WorldwideDay::new(20_241_220),
                    issued_units: ISSUED_UNITS as u32,
                    promis_load_minor: 1_500_000,
                    entry_price_minor: U256::from(2_000_000),
                    call_price_minor: U256::from(4_560_000),
                    call_trigger: IntexCallTrigger {
                        call_window_seconds: 1,
                        call_threshold_seconds: 1,
                        call_notice_period_seconds: 1,
                        transit_margin_seconds: 0,
                    },
                    issued_at: TIMESTAMP,
                    issuance_currency: 840,
                    reference_currency: 840,
                },
            )
            .unwrap();
        });
        let mut world = Self {
            evm: EvmWorld::new(db).at(TIMESTAMP),
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

    /// Initialized in place: the admin grants the relayer and auction roles to the issuer
    /// and the burner role to the factory precompile.
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
            ("AUCTION_ROLE", RELAYER),
            ("BURNER_ROLE", INTEX_FACTORY_ADDRESS),
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
                    issuedAt: TIMESTAMP,
                    issuanceCurrency: 840,
                    referenceCurrency: 840,
                    issuedUnits: ISSUED_UNITS as u32,
                    promisLoadMinor: 1_500_000,
                    entryPriceMinor: 2_000_000,
                    callPriceMinor: 4_560_000,
                    callTrigger: IIntexNFT1155::IntexCallTrigger {
                        callWindow: 1,
                        callThreshold: 1,
                        callNoticePeriod: 1,
                        transitMargin: 0,
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
        self.evm.call(caller, target, calldata, is_static)
    }

    fn ok<C: SolCall>(&mut self, caller: Address, target: Address, call: C) -> C::Return {
        self.evm.ok(caller, target, call)
    }

    fn view<C: SolCall>(&mut self, target: Address, call: C) -> C::Return {
        self.evm.view(ALICE, target, call)
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

    fn settled_balance(&mut self, account: Address) -> u64 {
        self.view(
            INTEX_ADDRESS,
            IIntex::balanceOfCall {
                account,
                id: issued_id(),
            },
        )
        .to::<u64>()
    }

    /// `[issued Alice, issued Bob, settled Alice, settled Bob]`.
    fn holdings(&mut self) -> [u64; 4] {
        [
            self.nft_balance(ALICE, issued_id()),
            self.nft_balance(BOB, issued_id()),
            self.settled_balance(ALICE),
            self.settled_balance(BOB),
        ]
    }

    /// `[issued supply on the NFT, settled units in the Intex registry]`.
    fn supplies(&mut self) -> [u64; 2] {
        let issued = self
            .view(
                INTEX_NFT1155_ADDRESS,
                INft::totalSupplyCall {
                    tokenId: issued_id(),
                },
            )
            .to::<u64>();
        let settled = self
            .view(
                INTEX_ADDRESS,
                IIntex::getIntexSeriesCall {
                    seriesId: FixedBytes(SERIES),
                },
            )
            .settledUnits;
        [issued, u64::from(settled)]
    }

    fn asset_balance(&mut self, account: Address) -> U256 {
        self.view(ASSET, IErc20Fixture::balanceOfCall { account })
    }

    fn settled_events(&self) -> Vec<(Address, u64)> {
        self.evm
            .logs()
            .iter()
            .filter_map(|log| IIntexFactory::IntexSettled::decode_log_data(&log.data).ok())
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
    let logs = world.evm.logs().to_vec();
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
        assert_eq!(world.evm.logs(), logs, "{owner:?} x{units}");
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
