//! Borrow signed transaction data into the shared ZeroFee policy views.
use crate::{BootstrapTransactionView, ZeroFeeTransaction};
use alloy_consensus::Transaction;
use alloy_primitives::Address;

impl<'a> ZeroFeeTransaction<'a> {
    /// Adapt an immutable transaction; the caller supplies its recovered signer.
    pub fn from_transaction<T: Transaction + ?Sized>(tx: &'a T, signer: Address) -> Self {
        Self {
            signer,
            to: tx.to(),
            value: tx.value(),
            input: tx.input().as_ref(),
            gas_limit: tx.gas_limit(),
            max_fee_per_gas: tx.max_fee_per_gas(),
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas(),
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
            to: transaction.to,
            value: transaction.value,
            input: transaction.input,
            gas_limit: transaction.gas_limit,
            max_fee_per_gas: transaction.max_fee_per_gas,
            max_priority_fee_per_gas: transaction.max_priority_fee_per_gas,
            access_list_empty: tx.access_list().is_some_and(|list| list.is_empty()),
            authorization_list,
        })
    }
}
