//! `outbe-cli tee` — TEE offer-key registration for a joining validator.
//!
//! Pre-start flow: before launching `outbe-chain node` on a
//! TEE-bootstrapped chain, the joiner registers its enclave on-chain
//! (`registerEnclave`), reads the deterministically-sealed offer key from its OWN
//! tx receipt (the `OfferKeySealed` event), and installs it in its enclave. Only
//! then can the node execute offer blocks. Mirrors `secretd tx register auth` +
//! `q register seed` + `configure-secret`, run before `secretd start`.

use std::time::Duration;

use alloy_primitives::{B256, U256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use clap::Subcommand;
use eyre::{Result, WrapErr};
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::{EnclaveClient, QuotePolicy};

use crate::abi::{self, ITeeRegistry};
use crate::rpc::Rpc;

#[derive(Subcommand)]
pub enum TeeCmd {
    /// Register this node's enclave on-chain and install the offer key it is sealed.
    /// Run BEFORE `outbe-chain node` when joining a running TEE-bootstrapped chain.
    Join {
        /// Enclave sidecar endpoint: a UDS path or a `host:port` (Gramine) address.
        #[arg(long)]
        enclave_socket: String,
        /// Seconds to wait for the on-chain `OfferKeySealed` receipt event.
        #[arg(long, default_value_t = 60)]
        timeout_secs: u64,
    },
    /// Print this enclave's resident tribute-offer public key (the key clients
    /// encrypt offers to once DKG completes) and its DKG identity key. With
    /// `--diff-chain`, also read the on-chain registry `tributeOfferPublicKey()`
    /// and assert it MATCHES the enclave — exits non-zero on a registry-vs-enclave
    /// mismatch, so it can gate scripts.
    Pubkey {
        /// Enclave sidecar endpoint: a UDS path or a `host:port` (Gramine) address.
        #[arg(long)]
        enclave_socket: String,
        /// Also read the on-chain `tributeOfferPublicKey()` (TEE registry slot-1)
        /// and assert it equals the enclave's resident offer key.
        #[arg(long, default_value_t = false)]
        diff_chain: bool,
    },
}

impl TeeCmd {
    pub async fn run(self, client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
        match self {
            TeeCmd::Join {
                enclave_socket,
                timeout_secs,
            } => join(client, private_key, &enclave_socket, timeout_secs).await,
            TeeCmd::Pubkey {
                enclave_socket,
                diff_chain,
            } => pubkey(client, &enclave_socket, diff_chain).await,
        }
    }
}

/// Query the enclave's resident offer + identity public keys and (optionally)
/// diff the offer key against the on-chain registry. The resident offer key is
/// the Seam-F group key once DKG completes (else the pre-DKG dev key), so a
/// `--diff-chain` MISMATCH means the registry slot-1 value and the key the enclave
/// would actually decrypt offers with have diverged.
async fn pubkey(
    client: &(impl Rpc + Sync),
    enclave_socket: &str,
    diff_chain: bool,
) -> Result<()> {
    let mut enclave =
        EnclaveClient::connect_endpoint(enclave_socket, &QuotePolicy::dev_accept_any())
            .map_err(|e| eyre::eyre!("connect enclave at {enclave_socket}: {e}"))?;
    let label = enclave.attestation_label().to_string();
    let (mrenclave, mrsigner, isv_svn) = enclave.measurements();
    let remote_attested = enclave.is_hardware_attested();
    let (offer_pub, tee_bls_pub) = match enclave.request(&EnclaveRequest::GetPublicKeys) {
        Ok(EnclaveResponse::PublicKeys {
            recipient_x25519_pub,
            tee_bls_pub,
            ..
        }) => (recipient_x25519_pub, tee_bls_pub),
        Ok(other) => return Err(eyre::eyre!("expected enclave PublicKeys, got {other:?}")),
        Err(e) => return Err(eyre::eyre!("enclave GetPublicKeys failed: {e}")),
    };
    println!("enclave offer pubkey (recipient_x25519): 0x{}", hex::encode(offer_pub));
    println!("enclave tee_bls_pub (DKG identity):      0x{}", hex::encode(&tee_bls_pub));
    println!("attestation:                             {label}");
    println!("remote-attested (real quote):            {remote_attested}");
    println!("mrenclave:                               0x{}", hex::encode(mrenclave));
    println!("mrsigner:                                0x{}", hex::encode(mrsigner));
    println!("isv_svn:                                 {isv_svn}");
    if !diff_chain {
        return Ok(());
    }

    let onchain = call_u256(
        client,
        ITeeRegistry::tributeOfferPublicKeyCall {}.abi_encode(),
    )
    .await?;
    if onchain.is_zero() {
        return Err(eyre::eyre!(
            "on-chain tributeOfferPublicKey == 0 — chain is not TEE-bootstrapped yet, \
             nothing to diff against"
        ));
    }
    let onchain_bytes: [u8; 32] = onchain.to_be_bytes();
    println!(
        "on-chain tributeOfferPublicKey (slot-1): 0x{}",
        hex::encode(onchain_bytes)
    );
    if onchain_bytes == offer_pub {
        println!("✓ MATCH — enclave resident offer key == on-chain registry");
        Ok(())
    } else {
        Err(eyre::eyre!(
            "✗ MISMATCH — enclave offer key 0x{} != on-chain 0x{}",
            hex::encode(offer_pub),
            hex::encode(onchain_bytes)
        ))
    }
}

async fn join(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    enclave_socket: &str,
    timeout_secs: u64,
) -> Result<()> {
    let signer = super::require_signer(private_key)?;
    let validator = signer.address();

    // 1. Connect to our own enclave and read its registration keys from the quote.
    let mut enclave =
        EnclaveClient::connect_endpoint(enclave_socket, &QuotePolicy::dev_accept_any())
            .map_err(|e| eyre::eyre!("connect enclave at {enclave_socket}: {e}"))?;
    let (recipient_x25519, attestation_pub, noise_static_pub, mrenclave, mrsigner, isv_svn) =
        match enclave.quote() {
            EnclaveResponse::Quote {
                recipient_x25519_pub,
                attestation_pub,
                noise_static_pub,
                mrenclave,
                mrsigner,
                isv_svn,
                ..
            } => (
                *recipient_x25519_pub,
                *attestation_pub,
                *noise_static_pub,
                *mrenclave,
                *mrsigner,
                *isv_svn,
            ),
            other => return Err(eyre::eyre!("expected enclave Quote, got {other:?}")),
        };
    println!(
        "enclave recipient_x25519: 0x{}",
        hex::encode(recipient_x25519)
    );

    // 2. Read the on-chain offer key + epoch (the joiner verifies the installed key
    //    against `expected_tribute_offer_public`) and the chain id.
    let offer_pub_u256 = call_u256(
        client,
        ITeeRegistry::tributeOfferPublicKeyCall {}.abi_encode(),
    )
    .await?;
    if offer_pub_u256.is_zero() {
        return Err(eyre::eyre!(
            "chain has no bootstrapped tribute offer key (tributeOfferPublicKey == 0) — \
             not a TEE chain, or it has not bootstrapped TEE yet"
        ));
    }
    let expected_offer_pub: [u8; 32] = offer_pub_u256.to_be_bytes();
    let tribute_offer_epoch =
        call_u256(client, ITeeRegistry::tributeOfferEpochCall {}.abi_encode())
            .await?
            .to::<u64>();
    let chain_id_u64 = client.eth_chain_id().await?;
    // Same B256 encoding the enclave bound the offer key to at bootstrap.
    let chain_id = B256::left_padding_from(&chain_id_u64.to_be_bytes());

    // 3. Submit registerEnclave from our EOA. The committee seals the offer key to
    //    our recipient key inside the tx and emits it.
    let from_block = client.eth_block_number().await?;
    let call = ITeeRegistry::registerEnclaveCall {
        recipientX25519: U256::from_be_bytes(recipient_x25519),
        attestationPub: U256::from_be_bytes(attestation_pub),
        noiseStaticPub: U256::from_be_bytes(noise_static_pub),
        mrenclave: U256::from_be_bytes(mrenclave.0),
        mrsigner: U256::from_be_bytes(mrsigner.0),
        isvSvn: isv_svn,
    }
    .abi_encode();
    let tx_hash = signer
        .send_tx(client, abi::TEE_REGISTRY_ADDR, call, U256::ZERO)
        .await
        .wrap_err("registerEnclave submission failed")?;
    println!("registerEnclave submitted: {tx_hash}");

    // 4. Poll for our OfferKeySealed event (filtered by topic0 + the validator topic).
    let topic0 = format!(
        "0x{}",
        hex::encode(ITeeRegistry::OfferKeySealed::SIGNATURE_HASH)
    );
    let topic1 = format!(
        "0x{}",
        hex::encode(B256::left_padding_from(validator.as_slice()))
    );
    let sealed = poll_offer_key_sealed(client, &topic0, &topic1, from_block, timeout_secs).await?;
    println!("offer key sealed blob received: {} bytes", sealed.len());

    // 5. Install the offer key in our enclave (write-once; the enclave accepts it
    //    ONLY if the derived public matches `expected_tribute_offer_public`).
    let resp = enclave
        .request(&EnclaveRequest::IngestTributeOfferHandoff {
            sealed,
            expected_tribute_offer_public: expected_offer_pub,
            chain_id,
            tribute_offer_epoch,
        })
        .map_err(|e| eyre::eyre!("enclave IngestTributeOfferHandoff failed: {e}"))?;
    match resp {
        EnclaveResponse::TributeOfferHandoffIngested {
            tribute_offer_public,
        } => {
            println!(
                "✓ offer key installed in enclave (offer_public 0x{}). \
                 You can now start `outbe-chain node`.",
                hex::encode(tribute_offer_public)
            );
            Ok(())
        }
        EnclaveResponse::Error { message } => {
            Err(eyre::eyre!("enclave rejected the offer key: {message}"))
        }
        other => Err(eyre::eyre!("unexpected enclave response: {other:?}")),
    }
}

/// `eth_call` a view returning a single `uint256`.
async fn call_u256(client: &(impl Rpc + Sync), call: Vec<u8>) -> Result<U256> {
    let result = client.eth_call(abi::TEE_REGISTRY_ADDR, &call).await?;
    U256::abi_decode(&result).wrap_err("decode uint256")
}

/// Poll `eth_getLogs` for the `OfferKeySealed` event matching our validator topic,
/// from the block before submission to head, until it appears or the timeout fires.
async fn poll_offer_key_sealed(
    client: &(impl Rpc + Sync),
    topic0: &str,
    topic1: &str,
    from_block: u64,
    timeout_secs: u64,
) -> Result<Vec<u8>> {
    let from = format!("0x{from_block:x}");
    let topics = [Some(topic0.to_string()), Some(topic1.to_string())];
    let deadline = Duration::from_secs(timeout_secs);
    let start = tokio::time::Instant::now();
    loop {
        let logs = client
            .eth_get_logs(abi::TEE_REGISTRY_ADDR, &topics, &from, "latest")
            .await
            .unwrap_or_default();
        if let Some(log) = logs.last() {
            let data_hex = log
                .get("data")
                .and_then(|d| d.as_str())
                .ok_or_else(|| eyre::eyre!("OfferKeySealed log has no data field"))?;
            let data = hex::decode(data_hex.trim_start_matches("0x"))
                .wrap_err("decode OfferKeySealed log data hex")?;
            // Event data ABI = `(bytes sealedOfferKey)`: [offset:32][length:32][blob..].
            let len = U256::from_be_slice(
                data.get(32..64)
                    .ok_or_else(|| eyre::eyre!("OfferKeySealed data too short for length"))?,
            )
            .to::<usize>();
            let blob = data
                .get(64..64 + len)
                .ok_or_else(|| eyre::eyre!("OfferKeySealed sealedOfferKey length out of bounds"))?
                .to_vec();
            return Ok(blob);
        }
        if start.elapsed() >= deadline {
            return Err(eyre::eyre!(
                "timed out after {timeout_secs}s waiting for the OfferKeySealed event \
                 (is the chain TEE-bootstrapped and the committee enclaves up?)"
            ));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
