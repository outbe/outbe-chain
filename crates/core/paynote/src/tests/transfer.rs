use super::*;
use crate::runtime::PAYNOTE_TRANSFER_TIMELOCK_SECS;
use crate::test_support::{change_note, claim_proof, locked_note, transfer_proof};

const NOW: u64 = 1_700_000_000;
const DEADLINE: u64 = NOW + PAYNOTE_TRANSFER_TIMELOCK_SECS;
const BOB_KEY: u64 = 21;
const ALICE_REFUND_KEY: u64 = 22;

#[test]
fn the_transfer_circuits_are_pinned() {
    use outbe_zk::paynote_claim::PaynoteClaim;
    use outbe_zk::paynote_transfer::PaynoteTransfer;
    assert_eq!(PaynoteTransfer::LABEL, "outbe.paynote.transfer");
    assert_eq!(PaynoteTransfer::VERSION, "1.0.0");
    assert_eq!(
        PaynoteTransfer::CIRCUIT_HASH,
        alloy_primitives::hex!("82b931931f5b32a97519b5b7a16381ac2776493dbcc224271f98929dbb6ffeda")
    );
    assert_eq!(
        PaynoteTransfer::VK_HASH,
        alloy_primitives::hex!("2a298d3c99eb1592ea78add2e7c849adb1af48515d52ad4dd722fbd6d16c3dee")
    );
    assert_eq!(PaynoteClaim::LABEL, "outbe.paynote.claim");
    assert_eq!(PaynoteClaim::VERSION, "1.0.0");
    assert_eq!(
        PaynoteClaim::CIRCUIT_HASH,
        alloy_primitives::hex!("4ed49f5cebfaba07a1436169f0afdfdcd25c996c07c9ffcd2dde99dd1aaec3be")
    );
    assert_eq!(
        PaynoteClaim::VK_HASH,
        alloy_primitives::hex!("8aa8965c7c0861018d974c13995eb9ae4f11a9dbfdad370e138f02cba07954f1")
    );
}

/// Alice locks 40 of a 100 note to Bob. The pool checks the deadline against
/// its clock, books the locked note and the change, and Bob's claim turns the
/// locked note into an ordinary one. The clock guards and replay are checked
/// on the way.
#[test]
fn a_transfer_locks_a_note_that_only_a_timely_proof_resolves() {
    let source = note(CHAIN_ID, 17, USDC, U256::from(100));
    let bob_pk = crate::hash::note_sn(Field::from(BOB_KEY)).unwrap();
    let locked = locked_note(
        CHAIN_ID,
        (bob_pk, ALICE_REFUND_KEY, 777),
        DEADLINE,
        USDC,
        U256::from(40),
    );
    let mut tree = new_tree(CHAIN_ID).unwrap();
    tree.append(source.commitment).unwrap();
    let transfer = transfer_proof(CHAIN_ID, &tree, 0, &source, &locked);

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    seed_pool(&mut provider, CHAIN_ID, tree.leaves());
    provider.enter(|storage| {
        // One second late leaves the recipient less than the timelock.
        storage.set_block_timestamp(U256::from(NOW + 1)).unwrap();
        assert_revert(
            runtime::transfer(&storage, &transfer),
            "PayNote transfer deadline is earlier than the 30-day timelock",
        );
        storage.set_block_timestamp(U256::from(NOW)).unwrap();
        let data = IPayNote::transferCall {
            proof: transfer.clone().into(),
        }
        .abi_encode();
        assert_eq!(base_gas(&data), crate::precompile::PAYNOTE_MERGE_BASE_GAS);
        dispatch(storage.clone(), &data, WBTC, U256::ZERO).unwrap(); // unrelated relayer
        let c: PayNoteContract<'_> = storage.contract();
        assert!(c.spent_nullifiers.read(&b256(source.nullifier)).unwrap());
        assert_eq!(c.leaf_count.read().unwrap(), 3);
        assert!(c.commitments.read(&b256(locked.commitment)).unwrap());
        assert_revert(
            runtime::transfer(&storage, &transfer),
            "PayNote nullifier has already been spent",
        );
    });
    tree.append(locked.commitment).unwrap();
    let change = change_note(CHAIN_ID, &source, locked.amount).unwrap();
    tree.append(change.commitment).unwrap();

    let claimed = note(CHAIN_ID, 23, USDC, locked.amount);
    // A claim dated one second ahead of the pool clock.
    let early = claim_proof(CHAIN_ID, &tree, 1, &locked, (BOB_KEY, NOW + 1), &claimed);
    let claim = claim_proof(CHAIN_ID, &tree, 1, &locked, (BOB_KEY, 0), &claimed);
    provider.enter(|storage| {
        assert_eq!(
            b256(tree.root()),
            storage
                .contract::<PayNoteContract<'_>>()
                .current_root
                .read()
                .unwrap()
        );
        assert_revert(
            runtime::resolve_transfer(&storage, &early),
            "PayNote claim not_before is in the future",
        );
        let data = IPayNote::resolveTransferCall {
            proof: claim.clone().into(),
        }
        .abi_encode();
        dispatch(storage.clone(), &data, WBTC, U256::ZERO).unwrap();
        let c: PayNoteContract<'_> = storage.contract();
        assert!(c.spent_nullifiers.read(&b256(locked.nullifier)).unwrap());
        assert!(c.commitments.read(&b256(claimed.commitment)).unwrap());
        assert_revert(
            runtime::resolve_transfer(&storage, &claim),
            "PayNote nullifier has already been spent",
        );
    });
}
