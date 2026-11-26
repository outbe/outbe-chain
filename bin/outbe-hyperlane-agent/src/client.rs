//! Outbe RPC access: read-only calls and signed transaction submission.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
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

/// The send lock of one chain. One transaction at a time reads its nonce and
/// goes to the pool of that chain. The liveness, the announce and the relayer
/// send with the same key `H`: without this lock, two of them can read the
/// same nonce. The lock is for each chain, so a slow endpoint of one chain
/// does not delay a transaction on another chain.
pub fn send_lock(chain_id: u64) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: Mutex<BTreeMap<u64, Arc<tokio::sync::Mutex<()>>>> = Mutex::new(BTreeMap::new());
    LOCKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(chain_id)
        .or_default()
        .clone()
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
///
/// The gas limit is the estimate of the node plus one half. The pool accepts
/// a transaction only when the balance of the sender is the gas limit times
/// the fee cap or more, also when ZeroFee takes no fee. A limit near the real
/// use keeps that balance low. A call that reverts fails here, at the
/// estimate, and not after the wait for a receipt.
pub async fn send_tx(sender: &TxSender<'_>, to: Address, calldata: &[u8]) -> Result<B256> {
    let provider = ProviderBuilder::new()
        .wallet(sender.wallet.clone())
        .connect_provider(sender.provider.clone());
    let mut tx = TransactionRequest::default()
        .from(sender.wallet.default_signer().address())
        .to(to)
        .input(Bytes::copy_from_slice(calldata).into());
    tx.set_chain_id(sender.chain_id);
    let gas = provider
        .estimate_gas(tx.clone())
        .await
        .map_err(|e| LivenessError::TransactionFailed(format!("gas estimate: {e:#}")))?;
    tx = tx.gas_limit(gas + gas / 2);
    if sender.gasless {
        let fee_cap = provider
            .get_gas_price()
            .await
            .unwrap_or(MIN_PROTOCOL_BASE_FEE as u128)
            .max(MIN_PROTOCOL_BASE_FEE as u128);
        tx = tx.max_fee_per_gas(fee_cap).max_priority_fee_per_gas(0);
    }
    let lock = send_lock(sender.chain_id);
    let _one_at_a_time = lock.lock().await;
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
