use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolInterface};

use outbe_fidelity::FidelityContract;
use outbe_gratis::Gratis;
use outbe_gratispool::constants::{denomination, ACTION_UNPLEDGE};
use outbe_gratispool::schema::GratisPoolContract;
use outbe_gratispool::state::{commitment_hash, nullifier_hash, receiver_binding};
use outbe_gratispool::verifier::with_verifier_outcome;
use outbe_primitives::addresses::CREDIS_ADDRESS;
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::{dispatch, IGratisFactory};
use crate::runtime;

const CHAIN_ID: u64 = 1;
const CREATED_AT: u64 = 1_700_000_000;

fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}

/// Gives `account` a positive RCFI by recording a gratis cohort acquired one
/// year before the test's block time. `pledge_gratis` gates on
/// `get_rcfi(caller) > 0`, so any test that expects a pledge to pass the
/// fidelity check must seed this first (and the storage timestamp must be set
/// so `get_rcfi` reads a non-zero `now`).
fn seed_fidelity(storage: StorageHandle<'_>, account: Address) {
    const ONE_YEAR_SECS: u64 = 365 * 86_400;
    let mut fidelity = FidelityContract::new(storage);
    fidelity
        .on_gratis_mined(account, U256::from(100u64), CREATED_AT - ONE_YEAR_SECS)
        .unwrap();
}

fn dispatch_call_bytes(call: IGratisFactory::IGratisFactoryCalls) -> Bytes {
    Bytes::from(call.abi_encode())
}

#[test]
fn pledge_moves_balance_into_escrow_and_credits_caller_ledger() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let amount = denomination(1).unwrap();
        Gratis::new(storage.clone())
            .mine(alice(), amount * U256::from(2u64))
            .unwrap();
        seed_fidelity(storage.clone(), alice());

        let pledge_call = dispatch_call_bytes(IGratisFactory::IGratisFactoryCalls::pledgeGratis(
            IGratisFactory::pledgeGratisCall {
                denomId: 1,
                commitment: U256::from(0xA1u64),
            },
        ));
        dispatch(storage.clone(), &pledge_call, alice(), U256::ZERO).unwrap();

        // Caller debited; per-pledger ledger credited; escrow holds amount.
        let gratis = Gratis::new(storage.clone());
        assert_eq!(gratis.balance_of(alice()).unwrap(), amount);
        assert_eq!(gratis.balance_of(CREDIS_ADDRESS).unwrap(), amount);
        assert_eq!(gratis.pledged_of(alice()).unwrap(), amount);

        // Commitment landed in the pool tree.
        let pool = GratisPoolContract::new(storage);
        assert_eq!(pool.leaf_count(1).unwrap(), 1);
    });
}

#[test]
fn pledge_unknown_denom_reverts() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        // Seed fidelity so the pledge clears the RCFI gate and reaches the
        // denomination check we're actually asserting here.
        seed_fidelity(storage.clone(), alice());

        let call = dispatch_call_bytes(IGratisFactory::IGratisFactoryCalls::pledgeGratis(
            IGratisFactory::pledgeGratisCall {
                denomId: 99,
                commitment: U256::from(0xA2u64),
            },
        ));
        let err = dispatch(storage, &call, alice(), U256::ZERO).unwrap_err();
        assert!(err.to_string().contains("denomination id out of range"));
    });
}

#[test]
fn pledge_duplicate_commitment_reverts() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let amount = denomination(1).unwrap();
        Gratis::new(storage.clone())
            .mine(alice(), amount * U256::from(2u64))
            .unwrap();
        seed_fidelity(storage.clone(), alice());

        let call = dispatch_call_bytes(IGratisFactory::IGratisFactoryCalls::pledgeGratis(
            IGratisFactory::pledgeGratisCall {
                denomId: 1,
                commitment: U256::from(0xA3u64),
            },
        ));
        dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        let err = dispatch(storage, &call, alice(), U256::ZERO).unwrap_err();
        assert!(err.to_string().contains("commitment already exists"));
    });
}

#[test]
fn unpledge_releases_escrow_back_to_pledger() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let amount = denomination(denom_id).unwrap();

        // Alice pledges.
        Gratis::new(storage.clone()).mine(alice(), amount).unwrap();
        seed_fidelity(storage.clone(), alice());
        let secret = U256::from(0xAAu64);
        let null_s = U256::from(0xBBu64);
        let commitment = commitment_hash(secret, null_s, denom_id).unwrap();
        // pledge_gratis returns the post-insert Merkle root; reuse it as the
        // spend proof's public input instead of re-reading from state.
        let (pledge_root, _, _) =
            runtime::pledge_gratis(storage.clone(), alice(), denom_id, commitment).unwrap();

        // Alice spends the pool commitment back to herself. The per-account
        // pledged ledger is keyed by `account`, so the unpledge destination
        // must match the pledger in the current PoC; the shielded part of
        // the design is the on-chain link between commitment and depositor,
        // not the destination address.
        let args = outbe_gratispool::SpendArgs {
            merkle_root: pledge_root,
            nullifier_hash: nullifier_hash(null_s).unwrap(),
            denom_id,
            receiver_binding: receiver_binding(ACTION_UNPLEDGE, alice(), CHAIN_ID, U256::ZERO)
                .unwrap(),
            proof: vec![0x00; 32],
        };
        let returned = with_verifier_outcome(true, || {
            runtime::unpledge_gratis(storage.clone(), &args, alice()).unwrap()
        });
        assert_eq!(returned, amount);

        // Gratis landed back at Alice; escrow drained; per-pledger ledger zero.
        let gratis = Gratis::new(storage);
        assert_eq!(gratis.balance_of(alice()).unwrap(), amount);
        assert_eq!(gratis.balance_of(CREDIS_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(gratis.pledged_of(alice()).unwrap(), U256::ZERO);
    });
}

#[test]
fn supports_interface() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = dispatch_call_bytes(IGratisFactory::IGratisFactoryCalls::supportsInterface(
            IGratisFactory::supportsInterfaceCall {
                interfaceId: alloy_primitives::FixedBytes(ERC165_INTERFACE_ID),
            },
        ));
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        assert!(IGratisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());

        let call = dispatch_call_bytes(IGratisFactory::IGratisFactoryCalls::supportsInterface(
            IGratisFactory::supportsInterfaceCall {
                interfaceId: alloy_primitives::FixedBytes([0xde, 0xad, 0xbe, 0xef]),
            },
        ));
        let out = dispatch(storage, &call, alice(), U256::ZERO).unwrap();
        assert!(!IGratisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());
    });
}

#[test]
fn rejects_msg_value() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = dispatch_call_bytes(IGratisFactory::IGratisFactoryCalls::pledgeGratis(
            IGratisFactory::pledgeGratisCall {
                denomId: 1,
                commitment: U256::from(0xA5u64),
            },
        ));
        let err = dispatch(storage, &call, alice(), U256::from(1u64)).unwrap_err();
        assert!(err.to_string().contains("non-payable"));
    });
}
