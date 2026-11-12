//! Outbe RPC access: read-only calls and signed transaction submission.

use std::time::Duration;

use alloy_eips::{eip1559::MIN_PROTOCOL_BASE_FEE, BlockId};
use alloy_network::{EthereumWallet, TransactionBuilder};
use alloy_primitives::{Address, Bytes, B256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use alloy_rpc_types::{TransactionReceipt, TransactionRequest};
use eyre::{Context, Result};

use crate::errors::LivenessError;

pub async fn eth_call<P: Provider>(
    provider: &P,
    to: Address,
    calldata: Vec<u8>,
    block: BlockId,
) -> Result<Bytes> {
    let tx = TransactionRequest::default()
        .to(to)
        .input(Bytes::copy_from_slice(&calldata).into());
    provider
        .call(tx)
        .block(block)
        .await
        .with_context(|| "eth_call failed")
}

pub struct TxSender<'a> {
    /// A provider of [`crate::sync::connect`], with its time limits.
    pub provider: &'a DynProvider,
    pub wallet: &'a EthereumWallet,
    pub chain_id: u64,
    pub gasless: bool,
}

/// Submits `calldata` to `to` as a signed EIP-1559 transaction and returns
/// its hash without waiting for inclusion. With `gasless`, the function shapes
/// the transaction for the ZeroFee txpool policy. The priority fee is zero. The
/// fee cap is at the protocol minimum, so Reth's public txpool checks still pass.
pub async fn send_tx(sender: &TxSender<'_>, to: Address, calldata: &[u8]) -> Result<B256> {
    let provider = ProviderBuilder::new()
        .wallet(sender.wallet.clone())
        .connect_provider(sender.provider.clone());
    let mut tx = TransactionRequest::default()
        .to(to)
        .input(Bytes::copy_from_slice(calldata).into())
        .gas_limit(1_000_000);
    tx.set_chain_id(sender.chain_id);
    if sender.gasless {
        let fee_cap = provider
            .get_gas_price()
            .await
            .unwrap_or(MIN_PROTOCOL_BASE_FEE as u128)
            .max(MIN_PROTOCOL_BASE_FEE as u128);
        tx = tx.max_fee_per_gas(fee_cap).max_priority_fee_per_gas(0);
    }
    let pending = provider
        .send_transaction(tx)
        .await
        .map_err(|e| LivenessError::TransactionFailed(format!("{e:#}")))?;
    Ok(*pending.tx_hash())
}

/// Polls the receipt of `hash` until it exists or `timeout` ends.
pub async fn wait_receipt(
    provider: &DynProvider,
    hash: B256,
    timeout: Duration,
) -> Result<TransactionReceipt> {
    let poll = async {
        loop {
            if let Some(receipt) = provider.get_transaction_receipt(hash).await? {
                return Ok::<_, eyre::Report>(receipt);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    tokio::time::timeout(timeout, poll)
        .await
        .map_err(|_| LivenessError::NoReceipt(hash))?
}
