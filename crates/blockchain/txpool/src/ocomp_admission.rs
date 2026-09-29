//! OCOMP system-carrier pool admission.
//!
//! Stateless classification happens before the ordinary Ethereum validator.
//! Authorization reads committed state and does not write it.

use alloy_primitives::{Address, B256, U256};
use outbe_metadosis::api::{verify_result_vote_carrier, ResultVoteCarrierAdmission};
use outbe_ocomp_protocol::system_carrier::OcompSystemCarrierCandidate;
use outbe_primitives::{
    error::PrecompileError,
    storage::{
        readonly::{ReadOnlyStorageProvider, StorageReader},
        StorageHandle,
    },
    system_tx::OcompLifecycleActivation,
};
use reth_chainspec::{ChainSpecProvider, EthChainSpec, EthereumHardforks};
use reth_evm::ConfigureEvm;
use reth_primitives_traits::transaction::error::InvalidTransactionError;
use reth_storage_api::{BlockNumReader, StateProvider, StateProviderFactory};
use reth_transaction_pool::{
    error::{InvalidPoolTransactionError, PoolTransactionError},
    validate::ValidTransaction,
    EthPoolTransaction, EthTransactionValidator, TransactionOrigin, TransactionValidationOutcome,
};
use std::any::Any;

pub(crate) fn validate<Client, Tx, Evm>(
    inner: &EthTransactionValidator<Client, Tx, Evm>,
    activation: OcompLifecycleActivation,
    origin: TransactionOrigin,
    transaction: Tx,
    candidate: OcompSystemCarrierCandidate,
) -> TransactionValidationOutcome<Tx>
where
    Client: StateProviderFactory
        + BlockNumReader
        + ChainSpecProvider<ChainSpec: EthChainSpec + EthereumHardforks>,
    Tx: EthPoolTransaction + alloy_consensus::Transaction,
    Evm: ConfigureEvm,
{
    let invalid = |transaction, reason: String| {
        TransactionValidationOutcome::Invalid(
            transaction,
            InvalidPoolTransactionError::other(OutbeOcompSystemCarrierPoolError(reason)),
        )
    };
    // `chain_info` reads one canonical head, so the hash and number cannot tear.
    // State is loaded for that hash: a later tip does not pair this height with
    // a different block. `best_block_number` and `latest` are separate reads.
    let canonical = match inner.client().chain_info() {
        Ok(info) => info,
        Err(error) => {
            return TransactionValidationOutcome::Error(*transaction.hash(), Box::new(error))
        }
    };
    let Some(next_block) = canonical.best_number.checked_add(1) else {
        return invalid(
            transaction,
            "OCOMP carrier next block height overflow".to_owned(),
        );
    };
    if !activation.is_active_at(next_block) {
        return invalid(
            transaction,
            format!("OCOMP system carrier is not active at next block {next_block}"),
        );
    }
    if !inner.eip1559() {
        return invalid(
            transaction,
            "EIP-1559 is not active for the OCOMP system carrier".to_owned(),
        );
    }
    if transaction.nonce() == u64::MAX {
        return TransactionValidationOutcome::Invalid(
            transaction,
            InvalidPoolTransactionError::Eip2681,
        );
    }
    let encoded_length = transaction.encoded_length();
    if encoded_length > inner.max_tx_input_bytes() {
        return TransactionValidationOutcome::Invalid(
            transaction,
            InvalidPoolTransactionError::OversizedData {
                size: encoded_length,
                limit: inner.max_tx_input_bytes(),
            },
        );
    }
    let gas_limit = transaction.gas_limit();
    if gas_limit > inner.block_gas_limit() {
        return TransactionValidationOutcome::Invalid(
            transaction,
            InvalidPoolTransactionError::ExceedsGasLimit(gas_limit, inner.block_gas_limit()),
        );
    }
    if transaction.chain_id() != Some(inner.chain_id()) {
        return TransactionValidationOutcome::Invalid(
            transaction,
            InvalidTransactionError::ChainIdMismatch.into(),
        );
    }
    let is_local = inner
        .local_transactions_config()
        .is_local(origin, transaction.sender_ref());
    if is_local {
        if let Some(tx_fee_cap_wei) = *inner.tx_fee_cap() {
            if tx_fee_cap_wei != 0 {
                let max_tx_fee_wei = transaction.cost().saturating_sub(transaction.value());
                if max_tx_fee_wei > tx_fee_cap_wei {
                    return TransactionValidationOutcome::Invalid(
                        transaction,
                        InvalidPoolTransactionError::ExceedsFeeCap {
                            max_tx_fee_wei: max_tx_fee_wei.saturating_to(),
                            tx_fee_cap_wei,
                        },
                    );
                }
            }
        }
    }

    let state = match inner.client().state_by_block_hash(canonical.best_hash) {
        Ok(state) => state,
        Err(error) => {
            return TransactionValidationOutcome::Error(*transaction.hash(), Box::new(error))
        }
    };
    let account = match state.basic_account(transaction.sender_ref()) {
        Ok(account) => account.unwrap_or_default(),
        Err(error) => {
            return TransactionValidationOutcome::Error(*transaction.hash(), Box::new(error))
        }
    };
    match inner.validate_sender_bytecode(&transaction, &account, &state) {
        Err(outcome) => return outcome,
        Ok(Err(error)) => return TransactionValidationOutcome::Invalid(transaction, error),
        Ok(Ok(())) => {}
    }
    if transaction.requires_nonce_check() {
        if let Err(error) = inner.validate_sender_nonce(&transaction, &account) {
            return TransactionValidationOutcome::Invalid(transaction, error);
        }
    }

    let reader = RethStateReader::new(&state);
    let mut provider = ReadOnlyStorageProvider::new(reader);
    let storage = StorageHandle::new(&mut provider);
    match candidate {
        OcompSystemCarrierCandidate::ResultVote { .. } => {
            let admission = verify_result_vote_carrier(
                storage,
                transaction.input().as_ref(),
                transaction.sender(),
                next_block,
                &outbe_ocomp_protocol::profile::poc_schema_limits(),
            );
            match result_vote_pool_decision(admission) {
                ResultVotePoolDecision::Admit => {}
                ResultVotePoolDecision::Reject(reason) => return invalid(transaction, reason),
                ResultVotePoolDecision::Temporary(error) => {
                    return TransactionValidationOutcome::Error(
                        *transaction.hash(),
                        Box::new(error),
                    )
                }
            }
        }
        OcompSystemCarrierCandidate::NodMaterialization { .. } => {
            match outbe_validatorset::contract::ValidatorSet::new(storage)
                .resolve_validator_for_role(
                    transaction.sender(),
                    outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp,
                ) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return invalid(
                        transaction,
                        "OCOMP carrier signer is not authorized for this action".to_owned(),
                    )
                }
                Err(error) => {
                    return invalid(
                        transaction,
                        format!("OCOMP carrier authorization failed: {error}"),
                    )
                }
            }
        }
    }

    TransactionValidationOutcome::Valid {
        balance: U256::MAX,
        state_nonce: account.nonce,
        bytecode_hash: account.bytecode_hash,
        transaction: ValidTransaction::new(transaction, None),
        propagate: match origin {
            TransactionOrigin::External => true,
            TransactionOrigin::Local => {
                inner
                    .local_transactions_config()
                    .propagate_local_transactions
            }
            TransactionOrigin::Private => false,
        },
        authorities: None,
    }
}

/// Pool action for one full result-vote admission verdict.
#[derive(Debug)]
pub(crate) enum ResultVotePoolDecision {
    /// Signer and vote are executable, including a closed window whose
    /// execution path already returns the deadline receipt.
    Admit,
    /// Permanent carrier fault. The pool marks the sender bad.
    Reject(String),
    /// Local, early, or still-open-but-due state. The pool must not blame the peer.
    Temporary(OcompCarrierTemporaryError),
}

/// Maps the shared verifier onto pool policy.
///
/// Only a typed invalid carrier becomes a bad transaction. Deadline-passed
/// votes stay admissible so execution can emit the existing soft receipt.
pub(crate) fn result_vote_pool_decision(
    admission: ResultVoteCarrierAdmission,
) -> ResultVotePoolDecision {
    match admission {
        ResultVoteCarrierAdmission::Valid { .. }
        | ResultVoteCarrierAdmission::DeadlinePassed { .. } => ResultVotePoolDecision::Admit,
        ResultVoteCarrierAdmission::InvalidCarrier { reason } => {
            ResultVotePoolDecision::Reject(reason)
        }
        ResultVoteCarrierAdmission::NotYetOpen { open_height } => {
            ResultVotePoolDecision::Temporary(OcompCarrierTemporaryError::NotYetOpen {
                open_height,
            })
        }
        // Begin closes one due window per block. This carrier cannot execute
        // on the current parent, and it is not a permanent fault.
        ResultVoteCarrierAdmission::DeadlineDueUnclosed { deadline_height } => {
            ResultVotePoolDecision::Temporary(OcompCarrierTemporaryError::DeadlineDueUnclosed {
                deadline_height,
            })
        }
        ResultVoteCarrierAdmission::StateUnavailable { source } => {
            ResultVotePoolDecision::Temporary(OcompCarrierTemporaryError::StateUnavailable {
                source,
            })
        }
        ResultVoteCarrierAdmission::CorruptCommittedState { source } => {
            ResultVotePoolDecision::Temporary(OcompCarrierTemporaryError::CorruptCommittedState {
                source,
            })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum OcompCarrierTemporaryError {
    #[error("OCOMP result vote is not open until height {open_height}")]
    NotYetOpen { open_height: u64 },
    #[error("OCOMP result vote window at height {deadline_height} is due and still open")]
    DeadlineDueUnclosed { deadline_height: u64 },
    #[error("OCOMP result vote state is unavailable: {source}")]
    StateUnavailable { source: PrecompileError },
    #[error("OCOMP committed state is corrupt: {source}")]
    CorruptCommittedState { source: PrecompileError },
}

/// Bridges Reth's state provider into Outbe's read-only precompile storage.
pub(crate) struct RethStateReader<'a, P> {
    state: &'a P,
}

impl<'a, P> RethStateReader<'a, P> {
    pub(crate) fn new(state: &'a P) -> Self {
        Self { state }
    }
}

impl<P> StorageReader for RethStateReader<'_, P>
where
    P: StateProvider,
{
    fn read_storage(&self, address: Address, key: B256) -> outbe_primitives::error::Result<U256> {
        self.state
            .storage(address, key)
            .map(|value| value.unwrap_or(U256::ZERO))
            .map_err(|e| {
                outbe_primitives::error::PrecompileError::Storage(format!("state read failed: {e}"))
            })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("OCOMP system carrier rejected: {0}")]
pub(crate) struct OutbeOcompSystemCarrierPoolError(pub(crate) String);

impl PoolTransactionError for OutbeOcompSystemCarrierPoolError {
    fn is_bad_transaction(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
