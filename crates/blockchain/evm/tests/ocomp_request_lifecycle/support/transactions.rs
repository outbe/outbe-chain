use super::super::*;

pub(in crate::lifecycle) type TestPool = Pool<
    MockTransactionValidator<EthPooledTransaction>,
    OutbeTransactionOrdering<EthPooledTransaction>,
    InMemoryBlobStore,
>;
pub(in crate::lifecycle) fn test_pool(transactions: Vec<EthPooledTransaction>) -> TestPool {
    let pool = Pool::new(
        MockTransactionValidator::default(),
        OutbeTransactionOrdering::default(),
        InMemoryBlobStore::default(),
        PoolConfig::default(),
    );
    for transaction in transactions {
        futures::executor::block_on(pool.add_transaction(TransactionOrigin::Local, transaction))
            .expect("fixture vote enters the production transaction pool");
    }
    pool
}

pub(in crate::lifecycle) fn validator_secret(validator_index: u8) -> B256 {
    B256::repeat_byte(validator_index.saturating_add(1))
}

pub(in crate::lifecycle) fn validator_sender(validator_index: u8) -> Address {
    OutbeEvmSigner::from_secret_bytes(validator_secret(validator_index).0)
        .expect("fixture validator EVM key is valid")
        .address()
}

pub(in crate::lifecycle) fn pooled_vote_transaction(
    input: Bytes,
    validator_index: u8,
) -> EthPooledTransaction {
    let transaction: Transaction = TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 30_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(METADOSIS_ADDRESS),
        value: U256::ZERO,
        access_list: Default::default(),
        input,
    }
    .into();
    let signature = sign_secp256k1_message(
        validator_secret(validator_index),
        transaction.signature_hash(),
    )
    .expect("fixture EVM vote signer");
    let signed = TransactionSigned::new_unhashed(transaction, signature);
    EthPooledTransaction::try_from_consensus(
        signed
            .try_into_recovered()
            .expect("fixture EVM vote sender recovers"),
    )
    .expect("fixture EVM vote converts to pooled transaction")
}

pub(in crate::lifecycle) fn vote_sender_balance() -> U256 {
    U256::from(100_000_000_000_000_000_000u128)
}

pub(in crate::lifecycle) fn saturated_user_secret() -> B256 {
    B256::repeat_byte(0xE1)
}

pub(in crate::lifecycle) fn saturated_user_sender() -> Address {
    OutbeEvmSigner::from_secret_bytes(saturated_user_secret().0)
        .expect("fixture saturated-user key is valid")
        .address()
}

pub(in crate::lifecycle) fn pooled_user_call(
    secret: B256,
    nonce: u64,
    to: Address,
    gas_limit: u64,
    input: Bytes,
) -> EthPooledTransaction {
    let transaction: Transaction = TxEip1559 {
        chain_id: CHAIN_ID,
        nonce,
        gas_limit,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(to),
        value: U256::ZERO,
        access_list: Default::default(),
        input,
    }
    .into();
    let signature =
        sign_secp256k1_message(secret, transaction.signature_hash()).expect("fixture user signer");
    let signed = TransactionSigned::new_unhashed(transaction, signature);
    EthPooledTransaction::try_from_consensus(
        signed
            .try_into_recovered()
            .expect("fixture user sender recovers"),
    )
    .expect("fixture user transaction converts to pooled transaction")
}

pub(in crate::lifecycle) fn pooled_saturated_user_transaction(nonce: u64) -> EthPooledTransaction {
    let transaction: Transaction = TxEip1559 {
        chain_id: CHAIN_ID,
        nonce,
        gas_limit: SATURATED_USER_TRANSACTION_GAS,
        max_fee_per_gas: 2_000_000_000,
        max_priority_fee_per_gas: 1_000_000_000,
        to: TxKind::Call(BURNER_ADDRESS),
        value: U256::ZERO,
        access_list: Default::default(),
        input: Bytes::new(),
    }
    .into();
    let signature = sign_secp256k1_message(saturated_user_secret(), transaction.signature_hash())
        .expect("fixture saturated-user signer");
    let signed = TransactionSigned::new_unhashed(transaction, signature);
    EthPooledTransaction::try_from_consensus(
        signed
            .try_into_recovered()
            .expect("fixture saturated-user sender recovers"),
    )
    .expect("fixture saturated-user transaction converts to pooled transaction")
}
