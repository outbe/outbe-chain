//! A one-time key with no balance sends a PayNote proof on the zero-fee path.
//! This is what keeps the holder's own address out of `tx.from`.

use super::eip7702_support::*;
use super::*;
use outbe_paynote::precompile::IPayNote;
use outbe_paynote::runtime::PAYNOTE_TRANSFER_TIMELOCK_SECS;
use outbe_paynote::test_support::{locked_note, note, seed_leaves, transfer_proof};
use outbe_system_common::addresses::{PAYNOTE_ADDRESS, ZEROFEE_ADDRESS};
use outbe_zerofee::precompile::IZeroFee::SponsorshipAuthorized;
use outbe_zerofee::FREE_TX_PAYNOTE_PROOF_GAS_LIMIT;

const TIMESTAMP: u64 = 1_775_001_600;

#[test]
fn an_unfunded_key_sends_a_paynote_transfer_for_free() {
    let paid = note(CHAIN_ID, 23, Address::ZERO, U256::from(1_000));
    let mut tree = outbe_paynote::client::new_tree(CHAIN_ID).unwrap();
    tree.append(paid.commitment).unwrap();
    let bob = outbe_paynote::hash::note_sn(outbe_paynote::Field::from(31_u64)).unwrap();
    let locked = locked_note(
        CHAIN_ID,
        (bob, 23, 7),
        TIMESTAMP + PAYNOTE_TRANSFER_TIMELOCK_SECS,
        Address::ZERO,
        U256::from(400),
    );
    let call = IPayNote::transferCall {
        proof: transfer_proof(CHAIN_ID, &tree, 0, &paid, &locked).into(),
    };
    let recovered = TransactionSigned::from(
        TxEip1559 {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit: FREE_TX_PAYNOTE_PROOF_GAS_LIMIT,
            max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(PAYNOTE_ADDRESS),
            value: U256::ZERO,
            input: call.abi_encode().into(),
            access_list: Default::default(),
        }
        .into_signed(Signature::test_signature()),
    )
    .try_into_recovered()
    .expect("test-signature must recover");
    let signer = Address::from(*recovered.signer());

    let mut state = cache_db_with_paymaster_account(signer, U256::ZERO);
    outbe_evm::test_support::seed_storage(&mut state, TIMESTAMP, OWNER, |storage| {
        seed_leaves(&storage, CHAIN_ID, &[paid.commitment]);
    });

    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let evm = config.evm_with_env(&mut state, pectra_evm_env(1));
    let mut ctx = execution_ctx(Some(1), Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    let mut executor = config.create_executor(evm, ctx);
    executor
        .execute_transaction(recovered)
        .expect("sponsored transfer should execute");

    let receipt = &executor.receipts()[0];
    assert!(receipt.success, "the transfer must succeed");
    let logged = |address: Address, signature: B256| {
        receipt
            .logs
            .iter()
            .any(|log| log.address == address && log.topics().first() == Some(&signature))
    };
    assert!(logged(
        ZEROFEE_ADDRESS,
        SponsorshipAuthorized::SIGNATURE_HASH
    ));
    assert!(logged(
        PAYNOTE_ADDRESS,
        IPayNote::NoteTransferred::SIGNATURE_HASH
    ));
    let output = executor.into_state();
    state.commit_source(&BundleSource(&output));
    assert_eq!(signer_balance(&mut state, signer), U256::ZERO);
}
