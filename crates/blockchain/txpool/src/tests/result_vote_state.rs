//! Pool admission of one persisted open result-vote job.
//!
//! Slots come from the shared Metadosis fixture. Carriers go through
//! `OutbeTransactionValidator`. Admission at `open_height - 1` and rejection
//! at `open_height - 2` pin inclusion to `best_number + 1`.

use std::collections::HashMap;

use alloy_consensus::{SignableTransaction as _, TxEip1559};
use alloy_eips::eip2718::Encodable2718 as _;
use alloy_primitives::{Address, Bytes, Signature, TxKind, B256, U256};
use outbe_metadosis::test_support::{persisted_open_result_vote, PersistedOpenResultVote};
use outbe_ocomp_protocol::{
    abi::METADOSIS_ADDRESS,
    system_carrier::{MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS, OCOMP_SYSTEM_CARRIER_GAS_LIMIT},
};
use reth_ethereum::TransactionSigned;
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
use reth_transaction_pool::{
    blobstore::InMemoryBlobStore,
    error::{InvalidPoolTransactionError, PoolTransactionError as _},
    validate::EthTransactionValidatorBuilder,
    EthPooledTransaction, TransactionOrigin, TransactionValidationOutcome, TransactionValidator,
};

use crate::{ocomp_admission, OcompLifecycleActivation, OutbeTransactionValidator};

fn carrier(sender: Address, input: Bytes) -> EthPooledTransaction {
    let tx: TransactionSigned = TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit: OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
        max_fee_per_gas: MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(METADOSIS_ADDRESS),
        value: U256::ZERO,
        input,
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into();
    let encoded_length = tx.encode_2718_len();
    let recovered = alloy_consensus::transaction::Recovered::new_unchecked(tx, sender);
    EthPooledTransaction::new(recovered, encoded_length)
}

fn provider_at(vote: &PersistedOpenResultVote, best_number: u64) -> MockEthProvider {
    let mut grouped: HashMap<Address, Vec<(B256, U256)>> = HashMap::new();
    for (address, slot, value) in &vote.slots {
        grouped.entry(*address).or_default().push((*slot, *value));
    }
    // `header_by_id(Latest)` resolves through the block map. A bare header
    // makes the best number this height and then returns no block.
    let provider = MockEthProvider::default().with_genesis_block();
    let hash = B256::from(U256::from(best_number).to_be_bytes());
    let header = alloy_consensus::Header {
        number: best_number,
        ..Default::default()
    };
    provider.add_block(hash, reth_ethereum::Block::new(header, Default::default()));

    let sender_storage = grouped.remove(&vote.signer).unwrap_or_default();
    provider.add_account(
        vote.signer,
        ExtendedAccount::new(0, U256::MAX).extend_storage(sender_storage),
    );
    for (address, storage) in grouped {
        provider.add_account(
            address,
            ExtendedAccount::new(0, U256::ZERO).extend_storage(storage),
        );
    }
    provider
}

fn validate(
    vote: &PersistedOpenResultVote,
    best_number: u64,
    input: Bytes,
) -> TransactionValidationOutcome<EthPooledTransaction> {
    let provider = provider_at(vote, best_number);
    let inner =
        EthTransactionValidatorBuilder::new(provider, reth_ethereum::evm::EthEvmConfig::mainnet())
            .build(InMemoryBlobStore::default());
    let validator = OutbeTransactionValidator::new(inner, OcompLifecycleActivation::at_block(1));
    futures::executor::block_on(
        validator.validate_transaction(TransactionOrigin::External, carrier(vote.signer, input)),
    )
}

#[test]
fn validator_admits_authorized_vote_and_rejects_tampered_signature() {
    let vote = persisted_open_result_vote();
    let parent = vote.open_height - 1;
    let valid = validate(&vote, parent, vote.valid_calldata.clone());
    assert!(
        matches!(valid, TransactionValidationOutcome::Valid { .. }),
        "an authorized vote at best+1 must be admitted, got {valid:?}"
    );

    let tampered = validate(&vote, parent, vote.tampered_calldata.clone());
    let TransactionValidationOutcome::Invalid(_, error) = tampered else {
        panic!("tampered inner signature must be invalid, got {tampered:?}");
    };
    let InvalidPoolTransactionError::Other(inner) = error else {
        panic!("tampered carrier must be the OCOMP pool error, got {error}");
    };
    let carrier_error = inner
        .as_any()
        .downcast_ref::<ocomp_admission::OutbeOcompSystemCarrierPoolError>()
        .expect("tampered carrier error type");
    assert!(carrier_error.is_bad_transaction());
}

#[test]
fn validator_does_not_blame_a_due_window_begin_has_not_closed() {
    let vote = persisted_open_result_vote();
    let outcome = validate(&vote, vote.due_height - 1, vote.valid_calldata.clone());
    let TransactionValidationOutcome::Error(_, error) = outcome else {
        panic!("a due unclosed window must stay temporary, got {outcome:?}");
    };
    assert!(error
        .downcast_ref::<ocomp_admission::OcompCarrierTemporaryError>()
        .is_some_and(|temporary| matches!(
            temporary,
            ocomp_admission::OcompCarrierTemporaryError::DeadlineDueUnclosed { deadline_height }
                if *deadline_height == vote.due_height
        )));
}

#[test]
fn validator_uses_best_number_plus_one() {
    let vote = persisted_open_result_vote();
    // One below the admitting parent: best+1 is still before open. best+2 would
    // land on open and admit this same carrier.
    let outcome = validate(&vote, vote.open_height - 2, vote.valid_calldata.clone());
    let TransactionValidationOutcome::Error(_, error) = outcome else {
        panic!("best+1 before the open height must stay temporary, got {outcome:?}");
    };
    assert!(error
        .downcast_ref::<ocomp_admission::OcompCarrierTemporaryError>()
        .is_some_and(|temporary| matches!(
            temporary,
            ocomp_admission::OcompCarrierTemporaryError::NotYetOpen { open_height }
                if *open_height == vote.open_height
        )));
}

#[test]
fn validator_reports_chain_info_failure_without_blaming_the_peer() {
    let vote = persisted_open_result_vote();
    let provider = provider_at(&vote, vote.open_height - 1);
    let inner = EthTransactionValidatorBuilder::new(
        provider.clone(),
        reth_ethereum::evm::EthEvmConfig::mainnet(),
    )
    .build(InMemoryBlobStore::default());
    // The builder already read the latest header. `chain_info` then scans this
    // shared header map, so emptying it fails that call and nothing later.
    provider.headers.lock().clear();
    let validator = OutbeTransactionValidator::new(inner, OcompLifecycleActivation::at_block(1));
    let outcome = futures::executor::block_on(validator.validate_transaction(
        TransactionOrigin::External,
        carrier(vote.signer, vote.valid_calldata),
    ));
    let TransactionValidationOutcome::Error(_, error) = outcome else {
        panic!("a chain_info failure must not be a bad transaction, got {outcome:?}");
    };
    assert!(matches!(
        error.downcast_ref::<reth_provider::ProviderError>(),
        Some(reth_provider::ProviderError::BestBlockNotFound)
    ));
    assert!(error
        .downcast_ref::<ocomp_admission::OutbeOcompSystemCarrierPoolError>()
        .is_none());
}
