//! ZeroFee paymaster commands.
//!
//! Currently exposes a single subcommand: signing an EIP-7702
//! [`Authorization`] tuple that delegates an EOA to the protocol
//! ZeroFee paymaster at
//! [`outbe_primitives::addresses::ZEROFEE_ADDRESS`]. The output is the
//! JSON-encoded `SignedAuthorization` that callers can drop into the
//! `authorizationList` field of a Pectra (type 0x04) transaction.
//!
//! The signing path is deliberately *offline-friendly*: it does not
//! contact the RPC node, so an operator can pre-sign authorizations
//! on an air-gapped machine and forward them to a sponsor service
//! over any transport.

use alloy_eips::eip7702::Authorization;
use alloy_primitives::{Address, U256};
use clap::Subcommand;
use eyre::Result;
use k256::ecdsa::signature::hazmat::PrehashSigner;
use serde::Serialize;

use crate::commands::require_signer;
use crate::rpc::RpcClient;
use crate::tx::TxSigner;

#[derive(Subcommand)]
pub enum ZeroFeeCmd {
    /// Sign an EIP-7702 Authorization delegating the EOA to the
    /// ZeroFee paymaster precompile so it can submit up to
    /// `FREE_TX_DAILY_LIMIT` free transactions per UTC day.
    ///
    /// The output JSON is the inner `SignedAuthorization` body — embed
    /// it in the `authorizationList` of a type-0x04 (Pectra) transaction.
    Eip7702Authorize {
        /// Target address the EOA delegates to. Defaults to the
        /// canonical `ZEROFEE_ADDRESS` precompile; the override
        /// exists for local-testnet scenarios where someone might
        /// re-deploy the paymaster behind a different address.
        #[arg(
            long,
            default_value_t = outbe_primitives::addresses::ZEROFEE_ADDRESS
        )]
        target: Address,

        /// Chain ID for the authorization. Set to 0 for the
        /// "any chain" form, which most production sponsors should
        /// avoid; the default reads from the configured RPC.
        #[arg(long)]
        chain_id: Option<u64>,

        /// EOA nonce to bind the authorization to. The signer's
        /// current nonce on the configured RPC is the safe default —
        /// override only if you know what you are doing.
        #[arg(long)]
        nonce: Option<u64>,
    },
}

impl ZeroFeeCmd {
    pub async fn run(self, client: &RpcClient, private_key: Option<&str>) -> Result<()> {
        match self {
            Self::Eip7702Authorize {
                target,
                chain_id,
                nonce,
            } => {
                let signer = require_signer(private_key)?;
                let chain_id = match chain_id {
                    Some(id) => id,
                    None => fetch_chain_id(client).await?,
                };
                let nonce = match nonce {
                    Some(n) => n,
                    None => fetch_nonce(client, signer.address()).await?,
                };
                sign_and_print_authorization(&signer, target, chain_id, nonce)
            }
        }
    }
}

/// Wire-format payload that drops verbatim into the `authorizationList`
/// field of a Pectra transaction. Field names match the EIP-7702 JSON
/// schema accepted by viem and `cast wallet sign-auth`. The recovered
/// signer address is intentionally absent — see
/// [`sign_and_print_authorization`] for the rationale.
#[derive(Serialize)]
struct SignedAuthorizationOutput {
    #[serde(rename = "chainId")]
    chain_id: U256,
    address: Address,
    #[serde(rename = "nonce")]
    nonce: u64,
    #[serde(rename = "yParity")]
    y_parity: u8,
    r: U256,
    s: U256,
}

fn sign_and_print_authorization(
    signer: &TxSigner,
    target: Address,
    chain_id: u64,
    nonce: u64,
) -> Result<()> {
    let auth = Authorization {
        chain_id: U256::from(chain_id),
        address: target,
        nonce,
    };
    let hash = auth.signature_hash();

    let (sig, recid): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signer
        .key()
        .sign_prehash(hash.as_slice())
        .map_err(|e| eyre::eyre!("EIP-7702 authorization signing failed: {e}"))?;

    let sig_bytes = sig.to_bytes();
    let r = U256::from_be_slice(&sig_bytes[..32]);
    let s = U256::from_be_slice(&sig_bytes[32..]);
    let y_parity = recid.to_byte();

    let output = SignedAuthorizationOutput {
        chain_id: auth.chain_id,
        address: auth.address,
        nonce: auth.nonce,
        y_parity,
        r,
        s,
    };

    // stdout carries the wire payload only — operators pipe it straight
    // into an `authorizationList` entry. The recovered signer address
    // goes to stderr so it cannot accidentally land in the JSON body
    // (viem 2.x silently ignores unknown fields, which would mask a
    // copy-paste mistake until the malformed tx hits the chain).
    eprintln!(
        "Signed EIP-7702 authorization for signer={} target={}",
        signer.address(),
        target
    );
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

async fn fetch_chain_id(client: &RpcClient) -> Result<u64> {
    use crate::rpc::Rpc;
    client.eth_chain_id().await
}

async fn fetch_nonce(client: &RpcClient, address: Address) -> Result<u64> {
    use crate::rpc::Rpc;
    client.eth_get_transaction_count(address).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn signed_output_serializes_with_camelcase_keys() {
        let out = SignedAuthorizationOutput {
            chain_id: U256::from(1u8),
            address: address!("0x000000000000000000000000000000000000ee09"),
            nonce: 7,
            y_parity: 1,
            r: U256::from(0x42u8),
            s: U256::from(0x43u8),
        };
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.contains("\"chainId\""));
        assert!(json.contains("\"yParity\""));
        assert!(json.contains("\"nonce\":7"));
        assert!(
            !json.contains("signer"),
            "signer field must NOT appear in wire payload (paste-into-tx footgun)"
        );
    }
}
