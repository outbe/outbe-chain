use crate::rpc::{self, Rpc};
use alloy_consensus::{SignableTransaction, TxEnvelope, TxLegacy};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{keccak256, Address, Signature, TxKind, U256};
use eyre::{ensure, Result};
use k256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey};
use serde_json::json;
use zeroize::Zeroizing;

pub struct Wallet {
    pub key: SigningKey,
    pub address: Address,
}
impl Wallet {
    pub fn new(key: &str) -> Result<Self> {
        let bytes = Zeroizing::new(
            hex::decode(key.strip_prefix("0x").unwrap_or(key))
                .map_err(|_| eyre::eyre!("Invalid --private-key"))?,
        );
        let key =
            SigningKey::from_slice(&bytes).map_err(|_| eyre::eyre!("Invalid --private-key"))?;
        let hash = keccak256(&key.verifying_key().to_encoded_point(false).as_bytes()[1..]);
        Ok(Self {
            key,
            address: Address::from_slice(&hash[12..]),
        })
    }

    pub async fn prepare(
        &self,
        rpc: &impl Rpc,
        to: Address,
        data: Vec<u8>,
        expected_chain: u64,
    ) -> Result<Vec<u8>> {
        self.prepare_value(rpc, to, data, U256::ZERO, expected_chain)
            .await
    }

    pub async fn prepare_value(
        &self,
        rpc: &impl Rpc,
        to: Address,
        data: Vec<u8>,
        value: U256,
        expected_chain: u64,
    ) -> Result<Vec<u8>> {
        ensure!(
            rpc::chain_id(rpc).await? == expected_chain,
            "RPC chain changed"
        );
        let nonce = rpc::quantity(
            &rpc.request("eth_getTransactionCount", json!([self.address, "pending"]))
                .await?,
        )?
        .try_into()?;
        let block = rpc::latest(rpc).await?;
        // Use the base fee, not a tip-inclusive suggestion that compounds on our own transactions.
        let fee = rpc::quantity(&block["baseFeePerGas"])?
            .checked_mul(U256::from(2))
            .ok_or_else(|| eyre::eyre!("Gas price overflow"))?
            .max(U256::from(1));
        let estimate: u64 = rpc::quantity(
            &rpc.request(
                "eth_estimateGas",
                json!([{
                    "from":self.address,"to":to,"data":format!("0x{}",hex::encode(&data)),
                    "value":format!("{value:#x}"),"gasPrice":format!("{fee:#x}")
                }]),
            )
            .await?,
        )?
        .try_into()?;
        let gas_limit = estimate
            .checked_add(estimate / 5)
            .ok_or_else(|| eyre::eyre!("Gas limit overflow"))?;
        if !value.is_zero() {
            let required = fee
                .checked_mul(U256::from(gas_limit))
                .and_then(|gas| gas.checked_add(value))
                .ok_or_else(|| eyre::eyre!("Transfer amount plus gas overflows uint256"))?;
            let balance = rpc::quantity(
                &rpc.request("eth_getBalance", json!([self.address, "pending"]))
                    .await?,
            )?;
            ensure!(
                balance >= required,
                "Insufficient RUDIS for amount plus gas"
            );
        }
        let tx = TxLegacy {
            chain_id: Some(expected_chain),
            nonce,
            gas_price: fee.try_into()?,
            gas_limit,
            to: TxKind::Call(to),
            value,
            input: data.into(),
        };
        let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = self
            .key
            .sign_prehash(tx.signature_hash().as_slice())
            .map_err(|_| eyre::eyre!("Transaction signing failed"))?;
        let signature = Signature::from_bytes_and_parity(
            signature.to_bytes().as_slice(),
            recovery.to_byte() != 0,
        )
        .normalized_s();
        Ok(TxEnvelope::Legacy(tx.into_signed(signature)).encoded_2718())
    }
}
