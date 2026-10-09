//! A paid holding's load mined into its owner's Promis or Gratis, and redeemed from
//! there into native COEN.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::units::checked_protocol_to_native;
use outbe_tee::protocol::{GratisOp, PromisOp};
use outbe_tee_enclave::confidential::ModifyOperation;
use outbe_tee_enclave::{gratis, promis};

use crate::features::settlement::{
    assert_mined_success, chain_id_b256, gratis_balance, promis_balance,
};
use crate::internal::{addresses, eth};
use crate::world::World;

/// The confidential balance a paid holding mines into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Ledger {
    Promis,
    Gratis,
}

impl Ledger {
    fn tee(self) -> outbe_tee::protocol::Ledger {
        match self {
            Self::Promis => outbe_tee::protocol::Ledger::Promis,
            Self::Gratis => outbe_tee::protocol::Ledger::Gratis,
        }
    }
}

/// What one owner mined, and the balance it was added to.
#[derive(Clone, Debug)]
pub(crate) struct Mined {
    pub(crate) owner: Address,
    pub(crate) owner_key: String,
    pub(crate) ledger: Ledger,
    pub(crate) before: U256,
    pub(crate) amount: U256,
}

/// One owner's redemption: the native balance either side, and the gas it paid.
#[derive(Clone, Debug)]
pub(crate) struct Redeemed {
    pub(crate) owner: Address,
    pub(crate) amount: U256,
    pub(crate) native_before: U256,
    pub(crate) native_after: U256,
    pub(crate) fee: U256,
}

pub(crate) fn balance(world: &World, ledger: Ledger, owner_key: &str, owner: Address) -> U256 {
    let url = world.rpc.url(world.validators.primary_port());
    let keys = eth::derive_account_keys(&url, owner_key, ledger.tee())
        .expect("derive the owner's confidential keys");
    match ledger {
        Ledger::Promis => promis_balance(&url, owner, &keys.view),
        Ledger::Gratis => gratis_balance(&url, owner, &keys.view),
    }
}

/// The owner's modify-key authorization to mint `amount` into `ledger`, bound to its
/// current operation nonce.
pub(crate) fn mint_authorization(
    world: &World,
    ledger: Ledger,
    owner_key: &str,
    owner: Address,
    amount: U256,
) -> (B256, u64) {
    authorization(
        world,
        &Grant {
            ledger,
            owner_key,
            owner,
            amount,
            mint: true,
        },
    )
}

/// One owner's mint or burn of `amount` on a confidential ledger.
struct Grant<'a> {
    ledger: Ledger,
    owner_key: &'a str,
    owner: Address,
    amount: U256,
    mint: bool,
}

impl Grant<'_> {
    fn operation<Op>(&self, op: Op, op_nonce: u64, chain_id: B256) -> ModifyOperation<Op> {
        ModifyOperation {
            account: self.owner,
            op,
            amount: self.amount,
            op_nonce,
            chain_id,
        }
    }
}

fn authorization(world: &World, grant: &Grant<'_>) -> (B256, u64) {
    let url = world.rpc.url(world.validators.primary_port());
    let keys = eth::derive_account_keys(&url, grant.owner_key, grant.ledger.tee())
        .expect("derive the owner's modify key");
    let chain_id = chain_id_b256(world);
    let nonce = match grant.ledger {
        Ledger::Promis => eth::read_call(
            &url,
            addresses::PROMIS_ADDR,
            &eth::IPromis::opNonceOfCall {
                account: grant.owner,
            },
        ),
        Ledger::Gratis => eth::read_call(
            &url,
            addresses::GRATIS_ADDR,
            &eth::IGratis::opNonceOfCall {
                account: grant.owner,
            },
        ),
    }
    .expect("operation nonce");
    let mac = match (grant.ledger, grant.mint) {
        (Ledger::Promis, true) => promis::modify_mac(
            &keys.modify,
            &grant.operation(PromisOp::Mint, nonce, chain_id),
        ),
        (Ledger::Promis, false) => promis::modify_mac(
            &keys.modify,
            &grant.operation(PromisOp::Burn, nonce, chain_id),
        ),
        (Ledger::Gratis, true) => gratis::modify_mac(
            &keys.modify,
            &grant.operation(GratisOp::Mint, nonce, chain_id),
        ),
        (Ledger::Gratis, false) => gratis::modify_mac(
            &keys.modify,
            &grant.operation(GratisOp::Burn, nonce, chain_id),
        ),
    };
    (B256::from(mac), nonce)
}

/// Each owner's balance grew by exactly what they mined.
pub(crate) fn assert_mined(world: &World, mined: &[Mined]) {
    for record in mined {
        assert_eq!(
            balance(world, record.ledger, &record.owner_key, record.owner),
            record.before + record.amount,
            "{} did not receive exactly the mined {:?} load",
            record.owner,
            record.ledger
        );
    }
}

/// Burn what each owner mined into native COEN, from the owner's own account.
pub(crate) fn redeem(world: &World, mined: &[Mined]) -> Vec<Redeemed> {
    let url = world.rpc.url(world.validators.primary_port());
    mined
        .iter()
        .map(|record| {
            let (mac, nonce) = authorization(
                world,
                &Grant {
                    ledger: record.ledger,
                    owner_key: &record.owner_key,
                    owner: record.owner,
                    amount: record.amount,
                    mint: false,
                },
            );
            let native_before = eth::balance(&url, record.owner).expect("native balance");
            let outcome = match record.ledger {
                Ledger::Promis => eth::send_call_outcome(
                    &url,
                    addresses::PROMIS_FACTORY_ADDR,
                    &record.owner_key,
                    &eth::IPromisFactory::mineCoenCall {
                        promisMinor: record.amount,
                        mac,
                        opNonce: nonce,
                    },
                    None,
                ),
                Ledger::Gratis => eth::send_call_outcome(
                    &url,
                    addresses::GRATIS_FACTORY_ADDR,
                    &record.owner_key,
                    &eth::IGratisFactory::mineCoenCall {
                        gratisMinor: record.amount,
                        mac,
                        opNonce: nonce,
                    },
                    None,
                ),
            }
            .expect("submit the COEN redemption");
            assert_mined_success(&outcome, "redeem mined load into COEN");
            let fee = crate::world::rpc::Rpc::receipt_gas_cost(&outcome.receipt)
                .expect("redemption gas cost");
            Redeemed {
                owner: record.owner,
                amount: record.amount,
                native_before,
                native_after: eth::balance(&url, record.owner).expect("native balance"),
                fee,
            }
        })
        .collect()
}

/// Each redemption burned the mined load back out of the ledger and paid out exactly
/// that load in native COEN, net of the gas the owner paid for it.
pub(crate) fn assert_redeemed(world: &World, mined: &[Mined], redeemed: &[Redeemed]) {
    for (record, redemption) in mined.iter().zip(redeemed) {
        assert_eq!(
            balance(world, record.ledger, &record.owner_key, record.owner),
            record.before,
            "{} still holds mined {:?} after redeeming it",
            record.owner,
            record.ledger
        );
        assert_eq!(
            redemption.native_after + redemption.fee,
            redemption.native_before
                + checked_protocol_to_native(redemption.amount).expect("load fits native COEN"),
            "{} was not paid exactly its redeemed load in native COEN",
            redemption.owner
        );
    }
}
