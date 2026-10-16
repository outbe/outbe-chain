//! ERC-20 settlement scenarios that every factory passes on the VaultRouter rail.
#![allow(dead_code)]

use std::fmt::Debug;

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::sol;
use outbe_evm::test_support::{install_code, runtime_code, seed_storage, EvmWorld};
use outbe_oracle::test_support::seed_trailing_vwap_window;
use outbe_primitives::{
    addresses::VAULT_ROUTER_ADDRESS,
    storage::{StorageHandle, SubCallOutput, SubCallStatus},
};
use outbe_vaultrouter::{
    api::IVaultRouter::StablesSource,
    test_support::{register_source, register_vault},
};
use revm::database::{CacheDB, EmptyDB};

sol! {
    interface IEurFixture {
        function configure(uint256 mode) external;
    }

    interface IFixture {
        function mint(address account, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function transferFrom(address from, address to, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
        function configure(uint256 mode, address factory, bytes reentry) external;
        function callbackRejected() external view returns (bool);
    }
}

pub const OWNER: Address = Address::new([0x11; 20]);
pub const ASSET: Address = Address::new([0x33; 20]);
pub const VAULT: Address = Address::new([0x55; 20]);
pub const THIRD_PARTY: Address = Address::new([0x77; 20]);
pub const TIMESTAMP: u64 = 1_700_000_000;
/// Funds the factory already holds; a failed or partial payment must never spend them.
pub const FACTORY_FLOAT: u64 = 17;

pub struct Quote {
    pub currency: u16,
    pub cost: U256,
    pub snapshot_id: U256,
}

/// One factory that settles its entities in ERC-20 through the VaultRouter.
pub trait Rail: Copy + Debug + Sized {
    const FACTORY: Address;
    const SOURCE: StablesSource;
    /// Addresses besides the token and vault that run the settlement fixture.
    const EXTRA_COUNTERPARTIES: &'static [Address] = &[];

    /// Issues and qualifies the entity `OWNER` settles. Returns its id and settlement deadline.
    fn seed(self, storage: &StorageHandle<'_>, issuance_currency: u16) -> (U256, u64);

    /// Funds the entity's own counterparties once the EVM runs.
    fn prepare(self, _world: &mut World<Self>) {}

    fn quote(self, world: &mut World<Self>) -> Quote;

    fn settle_call(self, id: U256, snapshot_id: U256) -> Bytes;

    fn assert_settled(self, world: &World<Self>);

    /// The settled entity still belongs to `OWNER` after a third party paid.
    fn assert_kept_by_owner(self, world: &mut World<Self>);
}

pub struct World<R: Rail> {
    pub evm: EvmWorld,
    pub rail: R,
    pub payer: Address,
    pub cost: U256,
    pub id: U256,
    /// The snapshot every settlement names: zero on the reference rail.
    pub snapshot_id: U256,
}

impl<R: Rail> World<R> {
    pub fn new(rail: R, payer: Address, registered: bool) -> Self {
        Self::build(rail, payer, registered, 840).0
    }

    /// The entity is issued in EUR (978) against a USD entry price. The Credis fixture token
    /// answers ISO 978 in mode 6, and the trailing window prices COEN at 2 USD and 1 EUR, so
    /// the cost is half as many EUR units as USD units.
    pub fn issuance_rail(rail: R, payer: Address) -> Self {
        Self::build(rail, payer, true, 978).0
    }

    /// Also returns the entity's settlement deadline.
    pub fn build(rail: R, payer: Address, registered: bool, issuance_currency: u16) -> (Self, u64) {
        let mut db = CacheDB::new(EmptyDB::default());
        let code = runtime_code(include_str!("../fixtures/evm/FactorySettlement.hex"));
        let eur_code = runtime_code(include_str!("../fixtures/evm/CredisIssuance.hex"));
        let token_code = if issuance_currency == 840 {
            &code
        } else {
            &eur_code
        };
        for addr in [ASSET, VAULT] {
            install_code(&mut db, addr, token_code);
        }
        for addr in R::EXTRA_COUNTERPARTIES {
            install_code(&mut db, *addr, &code);
        }
        let (id, deadline) = seed_storage(&mut db, TIMESTAMP, OWNER, |storage| {
            register_vault(&storage, ASSET, VAULT, issuance_currency).unwrap();
            if registered {
                register_source(&storage, R::FACTORY, R::SOURCE).unwrap();
            }
            let seeded = rail.seed(&storage, issuance_currency);
            if issuance_currency != 840 {
                seed_trailing_vwap_window(
                    &storage,
                    issuance_currency,
                    U256::from(2_000_000),
                    U256::from(1_000_000),
                )
                .unwrap();
            }
            seeded
        });
        let mut world = Self {
            evm: EvmWorld::new(db).at(TIMESTAMP),
            rail,
            payer,
            cost: U256::ZERO,
            id,
            snapshot_id: U256::ZERO,
        };
        if issuance_currency != 840 {
            world.ok(
                OWNER,
                ASSET,
                IEurFixture::configureCall {
                    mode: U256::from(6),
                },
            );
        }
        let quote = rail.quote(&mut world);
        assert_eq!(quote.currency, issuance_currency, "{rail:?}");
        assert!(!quote.cost.is_zero(), "{rail:?}");
        if issuance_currency == 840 {
            assert_eq!(quote.snapshot_id, U256::ZERO, "{rail:?}");
        }
        world.cost = quote.cost;
        world.snapshot_id = quote.snapshot_id;
        rail.prepare(&mut world);
        world.fund();
        (world, deadline)
    }

    fn fund(&mut self) {
        let (payer, cost) = (self.payer, self.cost);
        self.ok(
            OWNER,
            ASSET,
            IFixture::mintCall {
                account: payer,
                amount: cost * U256::from(2),
            },
        );
        self.ok(
            OWNER,
            ASSET,
            IFixture::mintCall {
                account: R::FACTORY,
                amount: U256::from(FACTORY_FLOAT),
            },
        );
        self.ok(
            payer,
            ASSET,
            IFixture::approveCall {
                spender: R::FACTORY,
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

    pub fn call(
        &mut self,
        caller: Address,
        target: Address,
        calldata: Bytes,
        is_static: bool,
    ) -> SubCallOutput {
        self.evm.call(caller, target, calldata, is_static)
    }

    pub fn ok<C: alloy_sol_types::SolCall>(
        &mut self,
        caller: Address,
        target: Address,
        call: C,
    ) -> C::Return {
        self.evm.ok(caller, target, call)
    }

    pub fn view<C: alloy_sol_types::SolCall>(&mut self, target: Address, call: C) -> C::Return {
        self.evm.view(OWNER, target, call)
    }

    pub fn settle(&mut self) -> SubCallOutput {
        self.settle_with(self.snapshot_id)
    }

    pub fn settle_with(&mut self, snapshot_id: U256) -> SubCallOutput {
        let calldata = self.rail.settle_call(self.id, snapshot_id);
        self.call(self.payer, R::FACTORY, calldata, false)
    }

    pub fn configure(&mut self, target: Address, mode: u64) {
        let reentry = self.rail.settle_call(self.id, self.snapshot_id);
        self.ok(
            OWNER,
            target,
            IFixture::configureCall {
                mode: U256::from(mode),
                factory: R::FACTORY,
                reentry,
            },
        );
    }

    /// `[payer, factory, router, vault]` token balances and the router's vault shares.
    pub fn balances(&mut self) -> [U256; 5] {
        let [payer, factory, router, vault] = [self.payer, R::FACTORY, VAULT_ROUTER_ADDRESS, VAULT]
            .map(|account| self.view(ASSET, IFixture::balanceOfCall { account }));
        let shares = self.view(
            VAULT,
            IFixture::balanceOfCall {
                account: VAULT_ROUTER_ADDRESS,
            },
        );
        [payer, factory, router, vault, shares]
    }

    pub fn allowances(&mut self) -> [U256; 3] {
        [
            (self.payer, R::FACTORY),
            (R::FACTORY, VAULT_ROUTER_ADDRESS),
            (VAULT_ROUTER_ADDRESS, VAULT),
        ]
        .map(|(owner, spender)| self.view(ASSET, IFixture::allowanceCall { owner, spender }))
    }

    pub fn paid_balances(&self) -> [U256; 5] {
        [
            self.cost,
            U256::from(FACTORY_FLOAT),
            U256::ZERO,
            self.cost,
            self.cost,
        ]
    }

    pub fn succeeds(&self, out: SubCallOutput) {
        assert!(
            matches!(out.status, SubCallStatus::Success),
            "{:?}: {:?}",
            self.rail,
            out.status
        );
    }
}

/// The quote names the trailing snapshot, a payment that names another snapshot moves
/// nothing, and the one that names it moves exactly the converted cost into the reserve.
pub fn issuance_currency_payment_moves_the_converted_cost<R: Rail>(rail: R) {
    let usd_cost = World::new(rail, OWNER, true).cost;
    let mut world = World::issuance_rail(rail, OWNER);
    assert_eq!(world.cost * U256::from(2), usd_cost, "{rail:?}");
    assert_ne!(world.snapshot_id, U256::ZERO, "{rail:?}");

    let unpaid = world.balances();
    assert!(
        !matches!(world.settle_with(U256::ZERO).status, SubCallStatus::Success),
        "{rail:?}"
    );
    assert_eq!(world.balances(), unpaid, "{rail:?}");

    let out = world.settle();
    world.succeeds(out);
    assert_eq!(world.balances(), world.paid_balances(), "{rail:?}");
    rail.assert_settled(&world);
}

pub fn quoted_cost_moves_into_the_reserve_once<R: Rail>(rail: R) {
    let mut world = World::new(rail, OWNER, true);
    let out = world.settle();
    world.succeeds(out);
    assert_eq!(world.balances(), world.paid_balances(), "{rail:?}");
    rail.assert_settled(&world);

    let balances = world.balances();
    assert!(
        !matches!(world.settle().status, SubCallStatus::Success),
        "{rail:?}"
    );
    assert_eq!(world.balances(), balances, "{rail:?}");
}

pub fn third_party_pays_and_the_entity_stays_with_the_owner<R: Rail>(rail: R) {
    let mut world = World::new(rail, THIRD_PARTY, true);
    let out = world.settle();
    world.succeeds(out);
    assert_eq!(world.balances(), world.paid_balances(), "{rail:?}");
    rail.assert_settled(&world);
    rail.assert_kept_by_owner(&mut world);
}

/// False transfer/approve/router transfer, vault revert, malformed bool, success without
/// movement, fee-on-transfer, and missing router authorization.
pub fn payment_failures_restore_balances_allowances_and_logs<R: Rail>(rail: R) {
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
        let mut world = World::new(rail, OWNER, registered);
        world.configure(target, mode);
        let balances = world.balances();
        let allowances = world.allowances();
        let logs = world.evm.logs().to_vec();
        assert!(
            !matches!(world.settle().status, SubCallStatus::Success),
            "{rail:?} mode {mode}"
        );
        assert_eq!(world.balances(), balances, "{rail:?} mode {mode}");
        assert_eq!(world.allowances(), allowances, "{rail:?} mode {mode}");
        assert_eq!(world.evm.logs(), logs, "{rail:?} mode {mode}");

        // The rollback left the entity settleable.
        world.configure(target, 0);
        if registered {
            let out = world.settle();
            world.succeeds(out);
        }
    }
}

pub fn optional_empty_token_returns_are_accepted<R: Rail>(rail: R) {
    let mut world = World::new(rail, OWNER, true);
    world.configure(ASSET, 7);
    let out = world.settle();
    world.succeeds(out);
    assert_eq!(world.balances(), world.paid_balances(), "{rail:?}");
}

pub fn the_payer_cannot_reenter_settlement_during_transfer<R: Rail>(rail: R) {
    let mut world = World::new(rail, ASSET, true);
    world.configure(ASSET, 5);
    let out = world.settle();
    world.succeeds(out);
    assert!(world.view(ASSET, IFixture::callbackRejectedCall {}));
    assert_eq!(world.balances(), world.paid_balances(), "{rail:?}");
}
