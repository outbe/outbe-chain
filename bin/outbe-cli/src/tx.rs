//! Transaction building and signing (EIP-155 legacy transactions).

use alloy_primitives::{keccak256, Address, U256};
use eyre::Result;
use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::SigningKey;

use crate::rpc::Rpc;

/// Buffer the suggested gas price so a legacy tx survives an EIP-1559 base-fee rise
/// between the `eth_gasPrice` read and inclusion. `eth_gasPrice` returns the current
/// base fee, but the begin-zone system txs (and offer decryption) make blocks bursty,
/// so the base fee can climb several steps before the tx lands — leaving a tx priced
/// exactly at the read-time base fee rejected as `gas price is less than basefee`.
/// The chain is ZeroFee, so over-pricing costs the sender nothing; a `2x` headroom
/// plus a 1-gwei floor (well above observed localnet base fees) keeps txs admittable.
pub(crate) fn buffered_gas_price(suggested: U256) -> U256 {
    suggested
        .saturating_mul(U256::from(2))
        .max(U256::from(1_000_000_000u64))
}

/// Transaction signer backed by a secp256k1 private key.
pub struct TxSigner {
    key: SigningKey,
    address: Address,
}

impl TxSigner {
    /// Create from a hex-encoded private key (with or without 0x prefix).
    pub fn new(private_key_hex: &str) -> Result<Self> {
        let hex_str = private_key_hex
            .strip_prefix("0x")
            .unwrap_or(private_key_hex);
        let key_bytes: [u8; 32] = hex::decode(hex_str)?.try_into().map_err(|bytes: Vec<u8>| {
            eyre::eyre!(
                "invalid private key length: expected 32 bytes, got {}",
                bytes.len()
            )
        })?;
        let key = SigningKey::from_bytes((&key_bytes).into())
            .map_err(|e| eyre::eyre!("invalid private key: {e}"))?;

        // Derive Ethereum address: keccak256(uncompressed_pubkey[1..])[-20..]
        let pubkey = key.verifying_key();
        let pubkey_point = pubkey.to_encoded_point(false);
        let hash = keccak256(&pubkey_point.as_bytes()[1..]);
        let address = Address::from_slice(&hash[12..]);

        Ok(Self { key, address })
    }

    pub fn address(&self) -> Address {
        self.address
    }

    /// Borrow the underlying secp256k1 signing key.
    ///
    /// Exposed so subcommands can sign payloads other than legacy
    /// transactions — e.g. the `keccak(0x05 || rlp(chain_id, address,
    /// nonce))` digest defined by EIP-7702. Keep new use-sites narrow.
    pub fn key(&self) -> &SigningKey {
        &self.key
    }

    /// Build, sign, and send a transaction to the given contract.
    pub async fn send_tx(
        &self,
        client: &(impl Rpc + Sync),
        to: Address,
        data: Vec<u8>,
        value: U256,
    ) -> Result<String> {
        let chain_id = client.eth_chain_id().await?;
        let nonce = client.eth_get_transaction_count(self.address).await?;
        let gas_price = buffered_gas_price(client.eth_gas_price().await?);
        let gas_limit = client.eth_estimate_gas(self.address, to, &data).await?;
        // Add 20% buffer to gas estimate
        let gas_limit = gas_limit
            .checked_add(gas_limit / 5)
            .ok_or_else(|| eyre::eyre!("gas limit buffer overflow"))?;

        let raw_tx =
            self.sign_legacy_tx(nonce, gas_price, gas_limit, to, value, &data, chain_id)?;

        client.eth_send_raw_transaction(&raw_tx).await
    }

    /// Build, sign, and send a transaction with an explicit gas limit, skipping
    /// `eth_estimateGas`. Useful when the call decrypts inside the enclave during
    /// execution and `eth_estimateGas` cannot faithfully simulate that path.
    pub async fn send_tx_with_gas(
        &self,
        client: &(impl Rpc + Sync),
        to: Address,
        data: Vec<u8>,
        value: U256,
        gas_limit: u64,
    ) -> Result<String> {
        let chain_id = client.eth_chain_id().await?;
        let nonce = client.eth_get_transaction_count(self.address).await?;
        let gas_price = buffered_gas_price(client.eth_gas_price().await?);
        let raw_tx =
            self.sign_legacy_tx(nonce, gas_price, gas_limit, to, value, &data, chain_id)?;
        client.eth_send_raw_transaction(&raw_tx).await
    }

    #[allow(clippy::too_many_arguments)]
    fn sign_legacy_tx(
        &self,
        nonce: u64,
        gas_price: U256,
        gas_limit: u64,
        to: Address,
        value: U256,
        data: &[u8],
        chain_id: u64,
    ) -> Result<Vec<u8>> {
        // EIP-155: hash [nonce, gasPrice, gasLimit, to, value, data, chainId, 0, 0]
        let mut unsigned = Vec::new();
        rlp_encode_list(
            &mut unsigned,
            &[
                &rlp_encode_u64(nonce),
                &rlp_encode_u256(gas_price),
                &rlp_encode_u64(gas_limit),
                &rlp_encode_bytes(to.as_slice()),
                &rlp_encode_u256(value),
                &rlp_encode_bytes(data),
                &rlp_encode_u64(chain_id),
                &rlp_encode_u64(0),
                &rlp_encode_u64(0),
            ],
        );

        let hash = keccak256(&unsigned);

        // ECDSA sign
        let (sig, recid): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = self
            .key
            .sign_prehash(hash.as_slice())
            .map_err(|e| eyre::eyre!("signing failed: {e}"))?;

        let sig_bytes = sig.to_bytes();
        let r = U256::from_be_slice(&sig_bytes[..32]);
        let s = U256::from_be_slice(&sig_bytes[32..]);
        let v = chain_id * 2 + 35 + recid.to_byte() as u64;

        // Encode signed tx: [nonce, gasPrice, gasLimit, to, value, data, v, r, s]
        let mut signed = Vec::new();
        rlp_encode_list(
            &mut signed,
            &[
                &rlp_encode_u64(nonce),
                &rlp_encode_u256(gas_price),
                &rlp_encode_u64(gas_limit),
                &rlp_encode_bytes(to.as_slice()),
                &rlp_encode_u256(value),
                &rlp_encode_bytes(data),
                &rlp_encode_u64(v),
                &rlp_encode_u256(r),
                &rlp_encode_u256(s),
            ],
        );

        Ok(signed)
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sign_legacy_tx_for_test(
        &self,
        nonce: u64,
        gas_price: U256,
        gas_limit: u64,
        to: Address,
        value: U256,
        data: &[u8],
        chain_id: u64,
    ) -> Result<Vec<u8>> {
        self.sign_legacy_tx(nonce, gas_price, gas_limit, to, value, data, chain_id)
    }
}

// ---------------------------------------------------------------------------
// Minimal RLP encoding (sufficient for legacy transaction encoding)
// ---------------------------------------------------------------------------

fn rlp_encode_u64(val: u64) -> Vec<u8> {
    if val == 0 {
        return vec![0x80]; // empty byte string
    }
    let bytes = val.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(7);
    let trimmed = &bytes[start..];
    if trimmed.len() == 1 && trimmed[0] < 0x80 {
        trimmed.to_vec()
    } else {
        let mut result = vec![0x80 + trimmed.len() as u8];
        result.extend_from_slice(trimmed);
        result
    }
}

fn rlp_encode_u256(val: U256) -> Vec<u8> {
    if val.is_zero() {
        return vec![0x80];
    }
    let bytes: [u8; 32] = val.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(31);
    let trimmed = &bytes[start..];
    if trimmed.len() == 1 && trimmed[0] < 0x80 {
        trimmed.to_vec()
    } else {
        let mut result = vec![0x80 + trimmed.len() as u8];
        result.extend_from_slice(trimmed);
        result
    }
}

fn rlp_encode_bytes(data: &[u8]) -> Vec<u8> {
    if data.len() == 1 && data[0] < 0x80 {
        data.to_vec()
    } else if data.len() < 56 {
        let mut result = vec![0x80 + data.len() as u8];
        result.extend_from_slice(data);
        result
    } else {
        let len_bytes = encode_length(data.len());
        let mut result = vec![0xb7 + len_bytes.len() as u8];
        result.extend_from_slice(&len_bytes);
        result.extend_from_slice(data);
        result
    }
}

fn rlp_encode_list(out: &mut Vec<u8>, items: &[&[u8]]) {
    let mut payload = Vec::new();
    for item in items {
        payload.extend_from_slice(item);
    }
    if payload.len() < 56 {
        out.push(0xc0 + payload.len() as u8);
    } else {
        let len_bytes = encode_length(payload.len());
        out.push(0xf7 + len_bytes.len() as u8);
        out.extend_from_slice(&len_bytes);
    }
    out.extend_from_slice(&payload);
}

fn encode_length(len: usize) -> Vec<u8> {
    let bytes = (len as u64).to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(7);
    bytes[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::mock::{ExpectedRpcCall, RecordedRpcCall, RecordedRpcResponse, RecordingRpc};

    // --- encode_length ---

    #[test]
    fn test_encode_length_single_byte() {
        assert_eq!(encode_length(1), vec![0x01]);
        assert_eq!(encode_length(55), vec![0x37]);
        assert_eq!(encode_length(56), vec![0x38]);
        assert_eq!(encode_length(255), vec![0xff]);
    }

    #[test]
    fn test_encode_length_two_bytes() {
        assert_eq!(encode_length(256), vec![0x01, 0x00]);
        assert_eq!(encode_length(65535), vec![0xff, 0xff]);
    }

    // --- rlp_encode_u64 ---

    #[test]
    fn test_rlp_encode_u64_zero() {
        assert_eq!(rlp_encode_u64(0), vec![0x80]);
    }

    #[test]
    fn test_rlp_encode_u64_single_byte() {
        assert_eq!(rlp_encode_u64(1), vec![0x01]);
        assert_eq!(rlp_encode_u64(0x7f), vec![0x7f]);
    }

    #[test]
    fn test_rlp_encode_u64_boundary_128() {
        assert_eq!(rlp_encode_u64(128), vec![0x81, 0x80]);
    }

    #[test]
    fn test_rlp_encode_u64_multi_byte() {
        assert_eq!(rlp_encode_u64(255), vec![0x81, 0xff]);
        assert_eq!(rlp_encode_u64(256), vec![0x82, 0x01, 0x00]);
        assert_eq!(rlp_encode_u64(1024), vec![0x82, 0x04, 0x00]);
    }

    #[test]
    fn test_rlp_encode_u64_max() {
        let encoded = rlp_encode_u64(u64::MAX);
        assert_eq!(encoded[0], 0x88); // 0x80 + 8 bytes
        assert_eq!(&encoded[1..], &[0xff; 8]);
    }

    // --- rlp_encode_u256 ---

    #[test]
    fn test_rlp_encode_u256_zero() {
        assert_eq!(rlp_encode_u256(U256::ZERO), vec![0x80]);
    }

    #[test]
    fn test_rlp_encode_u256_single_byte() {
        assert_eq!(rlp_encode_u256(U256::from(1u64)), vec![0x01]);
        assert_eq!(rlp_encode_u256(U256::from(0x7fu64)), vec![0x7f]);
    }

    #[test]
    fn test_rlp_encode_u256_boundary_128() {
        assert_eq!(rlp_encode_u256(U256::from(128u64)), vec![0x81, 0x80]);
    }

    #[test]
    fn test_rlp_encode_u256_multi_byte() {
        assert_eq!(rlp_encode_u256(U256::from(256u64)), vec![0x82, 0x01, 0x00]);
    }

    #[test]
    fn test_rlp_encode_u256_max() {
        let encoded = rlp_encode_u256(U256::MAX);
        assert_eq!(encoded[0], 0xa0); // 0x80 + 32
        assert_eq!(&encoded[1..], &[0xff; 32]);
    }

    // --- rlp_encode_bytes ---

    #[test]
    fn test_rlp_encode_bytes_empty() {
        assert_eq!(rlp_encode_bytes(&[]), vec![0x80]);
    }

    #[test]
    fn test_rlp_encode_bytes_single_low() {
        assert_eq!(rlp_encode_bytes(&[0x42]), vec![0x42]);
        assert_eq!(rlp_encode_bytes(&[0x7f]), vec![0x7f]);
    }

    #[test]
    fn test_rlp_encode_bytes_single_high() {
        assert_eq!(rlp_encode_bytes(&[0x80]), vec![0x81, 0x80]);
    }

    #[test]
    fn test_rlp_encode_bytes_20_byte_address() {
        let data = [0xab; 20];
        let encoded = rlp_encode_bytes(&data);
        assert_eq!(encoded[0], 0x94); // 0x80 + 20
        assert_eq!(&encoded[1..], &data);
    }

    #[test]
    fn test_rlp_encode_bytes_55_boundary() {
        let data = [0x01; 55];
        let encoded = rlp_encode_bytes(&data);
        assert_eq!(encoded[0], 0xb7); // 0x80 + 55, last short encoding
        assert_eq!(&encoded[1..], &data);
    }

    #[test]
    fn test_rlp_encode_bytes_56_long_encoding() {
        let data = [0x01; 56];
        let encoded = rlp_encode_bytes(&data);
        assert_eq!(encoded[0], 0xb8); // 0xb7 + 1 length byte
        assert_eq!(encoded[1], 56);
        assert_eq!(&encoded[2..], &data);
    }

    #[test]
    fn test_rlp_encode_bytes_256_long_encoding() {
        let data = [0x01; 256];
        let encoded = rlp_encode_bytes(&data);
        assert_eq!(encoded[0], 0xb9); // 0xb7 + 2 length bytes
        assert_eq!(&encoded[1..3], &[0x01, 0x00]);
        assert_eq!(&encoded[3..], &data);
    }

    // --- rlp_encode_list ---

    #[test]
    fn test_rlp_encode_list_empty() {
        let mut out = Vec::new();
        rlp_encode_list(&mut out, &[]);
        assert_eq!(out, vec![0xc0]);
    }

    #[test]
    fn test_rlp_encode_list_single_item() {
        let item = rlp_encode_u64(1);
        let mut out = Vec::new();
        rlp_encode_list(&mut out, &[&item]);
        assert_eq!(out, vec![0xc1, 0x01]);
    }

    #[test]
    fn test_rlp_encode_list_two_items() {
        let a = rlp_encode_u64(1);
        let b = rlp_encode_u64(2);
        let mut out = Vec::new();
        rlp_encode_list(&mut out, &[&a, &b]);
        assert_eq!(out, vec![0xc2, 0x01, 0x02]);
    }

    #[test]
    fn test_rlp_encode_list_long_payload() {
        // 60 bytes of 0x01 items → payload > 55
        let items: Vec<Vec<u8>> = (0..60).map(|_| rlp_encode_u64(1)).collect();
        let refs: Vec<&[u8]> = items.iter().map(|v| v.as_slice()).collect();
        let mut out = Vec::new();
        rlp_encode_list(&mut out, &refs);
        assert!(out[0] > 0xf7); // long list encoding
    }

    // --- TxSigner::send_tx ---

    const TEST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    const CHAIN_ID: u64 = 1337;
    const NONCE: u64 = 7;

    fn test_signer() -> TxSigner {
        TxSigner::new(TEST_KEY).unwrap()
    }

    fn tx_target() -> Address {
        "0x000000000000000000000000000000000000EE02"
            .parse()
            .unwrap()
    }

    fn tx_data() -> Vec<u8> {
        vec![0xde, 0xad, 0xbe, 0xef]
    }

    fn tx_value() -> U256 {
        U256::from(3u64)
    }

    fn gas_price() -> U256 {
        U256::from(9u64)
    }

    fn expected_raw_tx(signer: &TxSigner, gas_estimate: u64) -> Vec<u8> {
        let gas_limit = gas_estimate + gas_estimate / 5;
        signer
            .sign_legacy_tx_for_test(
                NONCE,
                buffered_gas_price(gas_price()),
                gas_limit,
                tx_target(),
                tx_value(),
                &tx_data(),
                CHAIN_ID,
            )
            .unwrap()
    }

    #[tokio::test]
    async fn test_send_tx_propagates_chain_id_error() {
        let signer = test_signer();
        let rpc = RecordingRpc::new([ExpectedRpcCall::err(
            RecordedRpcCall::EthChainId,
            "chain id unavailable",
        )]);

        let err = signer
            .send_tx(&rpc, tx_target(), tx_data(), tx_value())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("chain id unavailable"));
        assert_eq!(rpc.recorded_calls(), vec![RecordedRpcCall::EthChainId]);
        rpc.assert_done();
    }

    #[tokio::test]
    async fn test_send_tx_propagates_nonce_error() {
        let signer = test_signer();
        let rpc = RecordingRpc::new([
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthChainId,
                RecordedRpcResponse::U64(CHAIN_ID),
            ),
            ExpectedRpcCall::err(
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address(),
                },
                "nonce unavailable",
            ),
        ]);

        let err = signer
            .send_tx(&rpc, tx_target(), tx_data(), tx_value())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("nonce unavailable"));
        assert_eq!(
            rpc.recorded_calls(),
            vec![
                RecordedRpcCall::EthChainId,
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address()
                }
            ]
        );
        rpc.assert_done();
    }

    #[tokio::test]
    async fn test_send_tx_propagates_gas_price_error() {
        let signer = test_signer();
        let rpc = RecordingRpc::new([
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthChainId,
                RecordedRpcResponse::U64(CHAIN_ID),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address(),
                },
                RecordedRpcResponse::U64(NONCE),
            ),
            ExpectedRpcCall::err(RecordedRpcCall::EthGasPrice, "gas price unavailable"),
        ]);

        let err = signer
            .send_tx(&rpc, tx_target(), tx_data(), tx_value())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("gas price unavailable"));
        assert_eq!(
            rpc.recorded_calls(),
            vec![
                RecordedRpcCall::EthChainId,
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address()
                },
                RecordedRpcCall::EthGasPrice,
            ]
        );
        rpc.assert_done();
    }

    #[tokio::test]
    async fn test_send_tx_propagates_estimate_gas_error() {
        let signer = test_signer();
        let data = tx_data();
        let rpc = RecordingRpc::new([
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthChainId,
                RecordedRpcResponse::U64(CHAIN_ID),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address(),
                },
                RecordedRpcResponse::U64(NONCE),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGasPrice,
                RecordedRpcResponse::U256(gas_price()),
            ),
            ExpectedRpcCall::err(
                RecordedRpcCall::EthEstimateGas {
                    from: signer.address(),
                    to: tx_target(),
                    data: data.clone(),
                },
                "gas estimate unavailable",
            ),
        ]);

        let err = signer
            .send_tx(&rpc, tx_target(), data.clone(), tx_value())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("gas estimate unavailable"));
        assert_eq!(
            rpc.recorded_calls(),
            vec![
                RecordedRpcCall::EthChainId,
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address()
                },
                RecordedRpcCall::EthGasPrice,
                RecordedRpcCall::EthEstimateGas {
                    from: signer.address(),
                    to: tx_target(),
                    data,
                },
            ]
        );
        rpc.assert_done();
    }

    #[tokio::test]
    async fn test_send_tx_propagates_send_raw_transaction_error() {
        let signer = test_signer();
        let data = tx_data();
        let gas_estimate = 25;
        let raw_tx = expected_raw_tx(&signer, gas_estimate);
        let rpc = RecordingRpc::new([
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthChainId,
                RecordedRpcResponse::U64(CHAIN_ID),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address(),
                },
                RecordedRpcResponse::U64(NONCE),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGasPrice,
                RecordedRpcResponse::U256(gas_price()),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthEstimateGas {
                    from: signer.address(),
                    to: tx_target(),
                    data: data.clone(),
                },
                RecordedRpcResponse::U64(gas_estimate),
            ),
            ExpectedRpcCall::err(
                RecordedRpcCall::EthSendRawTransaction {
                    raw_tx: raw_tx.clone(),
                },
                "raw tx rejected",
            ),
        ]);

        let err = signer
            .send_tx(&rpc, tx_target(), data.clone(), tx_value())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("raw tx rejected"));
        assert_eq!(
            rpc.recorded_calls(),
            vec![
                RecordedRpcCall::EthChainId,
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address()
                },
                RecordedRpcCall::EthGasPrice,
                RecordedRpcCall::EthEstimateGas {
                    from: signer.address(),
                    to: tx_target(),
                    data,
                },
                RecordedRpcCall::EthSendRawTransaction { raw_tx },
            ]
        );
        rpc.assert_done();
    }

    #[tokio::test]
    async fn test_send_tx_applies_20_percent_gas_buffer() {
        let signer = test_signer();
        let data = tx_data();
        let gas_estimate = 25;
        let raw_tx = expected_raw_tx(&signer, gas_estimate);
        let rpc = RecordingRpc::new([
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthChainId,
                RecordedRpcResponse::U64(CHAIN_ID),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address(),
                },
                RecordedRpcResponse::U64(NONCE),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGasPrice,
                RecordedRpcResponse::U256(gas_price()),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthEstimateGas {
                    from: signer.address(),
                    to: tx_target(),
                    data: data.clone(),
                },
                RecordedRpcResponse::U64(gas_estimate),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthSendRawTransaction { raw_tx },
                RecordedRpcResponse::Text("0xok".to_string()),
            ),
        ]);

        let tx_hash = signer
            .send_tx(&rpc, tx_target(), data, tx_value())
            .await
            .unwrap();

        assert_eq!(tx_hash, "0xok");
        rpc.assert_done();
    }

    #[tokio::test]
    async fn test_send_tx_zero_gas_estimate_keeps_zero_gas_limit() {
        let signer = test_signer();
        let data = tx_data();
        let gas_estimate = 0;
        let raw_tx = expected_raw_tx(&signer, gas_estimate);
        let rpc = RecordingRpc::new([
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthChainId,
                RecordedRpcResponse::U64(CHAIN_ID),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address(),
                },
                RecordedRpcResponse::U64(NONCE),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGasPrice,
                RecordedRpcResponse::U256(gas_price()),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthEstimateGas {
                    from: signer.address(),
                    to: tx_target(),
                    data: data.clone(),
                },
                RecordedRpcResponse::U64(gas_estimate),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthSendRawTransaction { raw_tx },
                RecordedRpcResponse::Text("0xzero".to_string()),
            ),
        ]);

        let tx_hash = signer
            .send_tx(&rpc, tx_target(), data, tx_value())
            .await
            .unwrap();

        assert_eq!(tx_hash, "0xzero");
        rpc.assert_done();
    }

    #[tokio::test]
    async fn test_send_tx_gas_buffer_overflow_errors_before_submit() {
        let signer = test_signer();
        let data = tx_data();
        let rpc = RecordingRpc::new([
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthChainId,
                RecordedRpcResponse::U64(CHAIN_ID),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address(),
                },
                RecordedRpcResponse::U64(NONCE),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGasPrice,
                RecordedRpcResponse::U256(gas_price()),
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthEstimateGas {
                    from: signer.address(),
                    to: tx_target(),
                    data: data.clone(),
                },
                RecordedRpcResponse::U64(u64::MAX),
            ),
        ]);

        let err = signer
            .send_tx(&rpc, tx_target(), data.clone(), tx_value())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("gas limit buffer overflow"));
        assert_eq!(
            rpc.recorded_calls(),
            vec![
                RecordedRpcCall::EthChainId,
                RecordedRpcCall::EthGetTransactionCount {
                    address: signer.address()
                },
                RecordedRpcCall::EthGasPrice,
                RecordedRpcCall::EthEstimateGas {
                    from: signer.address(),
                    to: tx_target(),
                    data,
                },
            ]
        );
        rpc.assert_done();
    }

    // --- TxSigner::new ---

    #[test]
    fn test_tx_signer_roundtrip_with_prefix() {
        let signer =
            TxSigner::new("0x4c0883a69102937d6231471b5dbb6204fe512961708279f3d12b6fa1e4d0e3e6")
                .unwrap();
        // Same key without prefix must derive the same address
        let signer2 =
            TxSigner::new("4c0883a69102937d6231471b5dbb6204fe512961708279f3d12b6fa1e4d0e3e6")
                .unwrap();
        assert_eq!(signer.address(), signer2.address());
    }

    #[test]
    fn test_tx_signer_key_one() {
        let signer =
            TxSigner::new("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        let expected: Address = "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf"
            .parse()
            .unwrap();
        assert_eq!(signer.address(), expected);
    }

    #[test]
    fn test_tx_signer_different_keys_different_addresses() {
        let s1 = TxSigner::new("0000000000000000000000000000000000000000000000000000000000000001")
            .unwrap();
        let s2 = TxSigner::new("0000000000000000000000000000000000000000000000000000000000000002")
            .unwrap();
        assert_ne!(s1.address(), s2.address());
    }

    #[test]
    fn test_tx_signer_invalid_hex() {
        assert!(TxSigner::new("not-hex-at-all").is_err());
    }

    #[test]
    fn test_tx_signer_rejects_wrong_length_without_panicking() {
        for key in ["", "01", &"11".repeat(33)] {
            let result = std::panic::catch_unwind(|| TxSigner::new(key));
            assert!(
                result.is_ok(),
                "wrong-length key must return an error, not panic"
            );
            assert!(result.unwrap().is_err());
        }
    }

    #[test]
    fn test_tx_signer_zero_key() {
        assert!(
            TxSigner::new("0000000000000000000000000000000000000000000000000000000000000000")
                .is_err()
        );
    }
}
