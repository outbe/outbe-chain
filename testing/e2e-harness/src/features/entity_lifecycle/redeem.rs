//! A paid holding's load mined into its owner's Promis or Gratis, and redeemed from
//! there into native COEN.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::units::checked_protocol_to_native;
use outbe_tee::protocol::GratisOp;

use crate::features::settlement::{assert_mined_success, chain_id_b256, gratis_balance};
use crate::internal::{addresses, eth};
use crate::world::World;

/// The confidential balance a paid holding mines into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Ledger {
    Gratis,
}

impl Ledger {
    fn tee(self) -> outbe_tee::protocol::Ledger {
        match self {
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
    authorization(world, ledger, owner_key, owner, amount, true)
}

fn authorization(
    world: &World,
    ledger: Ledger,
    owner_key: &str,
    owner: Address,
    amount: U256,
    mint: bool,
) -> (B256, u64) {
    let url = world.rpc.url(world.validators.primary_port());
    let keys = eth::derive_account_keys(&url, owner_key, ledger.tee())
        .expect("derive the owner's modify key");
    let chain_id = chain_id_b256(world);
    match ledger {
        Ledger::Gratis => {
            let nonce = eth::read_call(
                &url,
                addresses::GRATIS_ADDR,
                &eth::IGratis::opNonceOfCall { account: owner },
            )
            .expect("Gratis operation nonce");
            let op = if mint { GratisOp::Mint } else { GratisOp::Burn };
            let mac = outbe_tee_enclave::gratis::modify_mac(
                &keys.modify,
                owner,
                op,
                amount,
                nonce,
                chain_id,
            );
            (B256::from(mac), nonce)
        }
    }
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
                record.ledger,
                &record.owner_key,
                record.owner,
                record.amount,
                false,
            );
            let native_before = eth::balance(&url, record.owner).expect("native balance");
            let outcome = match record.ledger {
                Ledger::Gratis => eth::send_call_outcome(
                    &url,
                    addresses::GRATIS_FACTORY_ADDR,
                    &record.owner_key,
                    &eth::IGratisFactory::mineCoenCall {
                        amount: record.amount,
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
