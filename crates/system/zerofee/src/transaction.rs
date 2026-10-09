//! Borrow signed transaction data into the shared ZeroFee policy views.
use crate::{BootstrapTransactionView, ZeroFeeTransaction};
use alloy_consensus::Transaction;
use alloy_primitives::Address;
use outbe_ocomp_protocol::transaction_call::TransactionCallFields;

impl<'a> ZeroFeeTransaction<'a> {
    /// Adapt an immutable transaction. The caller supplies its recovered signer.
    pub fn from_transaction<T: Transaction + ?Sized>(tx: &'a T, signer: Address) -> Self {
        Self {
            signer,
            call: TransactionCallFields::from_transaction(tx),
        }
    }
}

impl<'a> BootstrapTransactionView<'a> {
    /// Borrow the bootstrap envelope only when an authorization list exists.
    /// Classification and pre-state authorization remain separate policy steps.
    pub fn from_transaction<T: Transaction + ?Sized>(
        tx: &'a T,
        signer: Address,
        network_chain_id: u64,
    ) -> Option<Self> {
        let authorization_list = tx.authorization_list()?;
        let tx_chain_id = tx.chain_id();
        let nonce = tx.nonce();
        let transaction = ZeroFeeTransaction::from_transaction(tx, signer);
        Some(Self {
            signer,
            tx_chain_id,
            network_chain_id,
            nonce,
            call: transaction.call,
            access_list_empty: tx.access_list().is_some_and(|list| list.is_empty()),
            authorization_list,
        })
    }
}
