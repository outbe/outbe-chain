//! Outbe RPC access: read-only calls and signed transaction submission.

use alloy_eips::{eip1559::MIN_PROTOCOL_BASE_FEE, BlockId};
use alloy_network::{EthereumWallet, TransactionBuilder};
use alloy_primitives::{Address, Bytes, B256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types::TransactionRequest;
use alloy_signer_local::PrivateKeySigner;
use eyre::{Context, Result};

use crate::config::AccountConfig;

pub fn create_wallet(account: &AccountConfig) -> Result<EthereumWallet> {
    let signer: PrivateKeySigner = account
        .private_key
        .parse()
        .with_context(|| "invalid private key")?;
    Ok(EthereumWallet::from(signer))
}

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

/// Submits `calldata` to `to` as a signed EIP-1559 transaction and returns
/// its hash without waiting for inclusion. With `gasless`, the function shapes
/// the transaction for the ZeroFee txpool policy. The priority fee is zero. The
/// fee cap is at the protocol minimum, so Reth's public txpool checks still pass.
pub async fn send_tx(
    rpc_endpoint: &str,
    wallet: &EthereumWallet,
    chain_id: u64,
    to: Address,
    calldata: &[u8],
    gasless: bool,
) -> Result<B256> {
    let provider = ProviderBuilder::new()
        .wallet(wallet.clone())
        .connect_http(rpc_endpoint.parse().with_context(|| "invalid RPC URL")?);
    let mut tx = TransactionRequest::default()
        .to(to)
        .input(Bytes::copy_from_slice(calldata).into())
        .gas_limit(1_000_000);
    tx.set_chain_id(chain_id);
    if gasless {
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
        .map_err(|e| eyre::eyre!("transaction failed: {e:#}"))?;
    Ok(*pending.tx_hash())
}
