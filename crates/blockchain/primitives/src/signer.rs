//! Validator EVM signer system transaction artifacts.
//!
//! This signer is intentionally separate from the BLS consensus key and from
//! `header.beneficiary`. It signs deterministic unsigned transaction artifacts.
//! System tx EVM execution still runs with `SYSTEM_ADDRESS` as caller.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_consensus::{SignableTransaction as _, TxEip1559, TxLegacy};
use alloy_primitives::{keccak256, Address, Signature};
use k256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey};
use reth_ethereum::TransactionSigned;
use zeroize::Zeroizing;

pub mod load;

/// Validator EVM signer backed by a zeroizing secp256k1 secret.
#[derive(Clone)]
pub struct OutbeEvmSigner {
    identity: EvmSigningIdentity,
}

#[derive(Clone)]
struct EvmSigningIdentity {
    secret: Zeroizing<[u8; 32]>,
    address: Address,
}

impl EvmSigningIdentity {
    fn new(secret: [u8; 32]) -> Result<Self, SignerError> {
        let signing_key = signing_key_from_bytes(&secret)?;
        let address = address_from_signing_key(&signing_key);
        Ok(Self {
            secret: Zeroizing::new(secret),
            address,
        })
    }

    fn signing_key(&self) -> Result<SigningKey, SignerError> {
        signing_key_from_bytes(&self.secret)
    }
}

impl std::fmt::Debug for OutbeEvmSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutbeEvmSigner")
            .field("address", &self.address())
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl OutbeEvmSigner {
    pub fn from_secret_bytes(secret: [u8; 32]) -> Result<Self, SignerError> {
        Ok(Self {
            identity: EvmSigningIdentity::new(secret)?,
        })
    }

    pub fn from_hex(private_key_hex: &str) -> Result<Self, SignerError> {
        let hex = private_key_hex.trim();
        let hex = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes = hex::decode(hex).map_err(|error| SignerError::InvalidHex(error.to_string()))?;
        let len = bytes.len();
        let secret: [u8; 32] = bytes
            .try_into()
            .map_err(|_| SignerError::InvalidSecretLength { len })?;
        Self::from_secret_bytes(secret)
    }

    pub const fn address(&self) -> Address {
        self.identity.address
    }

    pub fn sign_unsigned(&self, tx: TxLegacy) -> Result<TransactionSigned, SignerError> {
        let signature = self.transaction_signature(&tx)?;
        Ok(tx.into_signed(signature).into())
    }

    /// Signs the restricted EIP-1559 envelope used by validator result votes.
    pub fn sign_eip1559(&self, tx: TxEip1559) -> Result<TransactionSigned, SignerError> {
        let signature = self.transaction_signature(&tx)?;
        Ok(tx.into_signed(signature).into())
    }

    fn transaction_signature<T: alloy_consensus::SignableTransaction<Signature>>(
        &self,
        tx: &T,
    ) -> Result<Signature, SignerError> {
        let signing_key = self.identity.signing_key()?;
        let hash = tx.signature_hash();
        let bytes = sign_recoverable_hash(&signing_key, &hash)?;
        Ok(Signature::from_bytes_and_parity(&bytes[..64], bytes[64] != 0).normalized_s())
    }

    /// Sign a raw 32-byte prehash and return a recoverable secp256k1 signature in
    /// `r(32) || s(32) || v(1)` form (`v` = recovery id 0/1). This is the exact
    /// format that [`crate::tee_signatures::recover_signer`] consumes. The
    /// consensus thread uses it to sign the TEE bootstrap payload's `signing_hash`
    /// with this validator's EVM key.
    pub fn sign_hash(&self, hash: &alloy_primitives::B256) -> Result<[u8; 65], SignerError> {
        let signing_key = self.identity.signing_key()?;
        sign_recoverable_hash(&signing_key, hash)
    }
}

fn sign_recoverable_hash(
    signing_key: &SigningKey,
    hash: &alloy_primitives::B256,
) -> Result<[u8; 65], SignerError> {
    let (signature, recovery_id): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing_key
        .sign_prehash(hash.as_slice())
        .map_err(|error| SignerError::SigningFailed(error.to_string()))?;
    let sig_bytes = signature.to_bytes();
    if sig_bytes.len() != 64 {
        return Err(SignerError::SignatureEncoding {
            len: sig_bytes.len(),
        });
    }
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig_bytes);
    out[64] = recovery_id.to_byte();
    Ok(out)
}

pub type SharedOutbeEvmSigner = Arc<OutbeEvmSigner>;

/// Default EVM-key path derived from the BLS signing-key path.
pub fn default_validator_evm_key_path(signing_key_path: &Path) -> PathBuf {
    signing_key_path
        .parent()
        .map(|parent| parent.join("evm-key.hex"))
        .unwrap_or_else(|| PathBuf::from("evm-key.hex"))
}

#[derive(Debug, thiserror::Error)]
pub enum SignerError {
    #[error("invalid EVM private key hex: {0}")]
    InvalidHex(String),
    #[error("invalid EVM private key length: {len} bytes (expected 32)")]
    InvalidSecretLength { len: usize },
    #[error("invalid EVM private key: {0}")]
    InvalidSecret(String),
    #[error("failed to read EVM private key from {path}: {source}", path = path.display())]
    ReadKey {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unsafe EVM private key permissions on {path}: mode {mode:o}; expected no group/other permissions", path = path.display())]
    UnsafeFilePermissions { path: PathBuf, mode: u32 },
    #[error("failed to inspect EVM private key permissions on {path}: {source}", path = path.display())]
    InspectPermissions {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unsafe EVM private key file at {path}: {reason}", path = path.display())]
    UnsafeKeyFile { path: PathBuf, reason: &'static str },
    #[error("EVM private key file does not contain a lowercase 32-byte hex key after trimming surrounding ASCII whitespace: {path}", path = path.display())]
    NonCanonicalKeyFile { path: PathBuf },
    #[error("failed to sign system transaction: {0}")]
    SigningFailed(String),
    #[error("unexpected ECDSA signature length: {len} bytes")]
    SignatureEncoding { len: usize },
}

fn signing_key_from_bytes(secret: &[u8; 32]) -> Result<SigningKey, SignerError> {
    SigningKey::from_bytes(secret.into())
        .map_err(|error| SignerError::InvalidSecret(error.to_string()))
}

fn address_from_signing_key(signing_key: &SigningKey) -> Address {
    let public_key = signing_key.verifying_key().to_encoded_point(false);
    let hash = keccak256(&public_key.as_bytes()[1..]);
    Address::from_slice(&hash[12..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_tx::{build_unsigned_system_tx, SystemTxInputV2, SystemTxKind};
    use alloy_consensus::Transaction as _;
    use alloy_primitives::{address, Bytes, TxKind, U256};
    use reth_primitives_traits::SignedTransaction as _;

    const CHAIN_ID: u64 = 2026;
    #[test]
    fn raw_signature_matches_fixed_prehash_vector() {
        let signer = OutbeEvmSigner::from_secret_bytes([1; 32]).unwrap();
        let signature = signer
            .sign_hash(&alloy_primitives::B256::with_last_byte(42))
            .unwrap();
        assert_eq!(
            hex::encode(signature),
            "85374ecb6e0ea7cb84429448bf06ca12ea17d9ff80d8fe910f25f2f4fead5b3442c30ba1656842a610ff1da4bf1823604bf15d3493b9043af65b7dced9bbed4400"
        );
    }

    #[test]
    fn eip1559_signature_matches_fixed_transaction_vector() {
        let signer = OutbeEvmSigner::from_secret_bytes([1; 32]).unwrap();
        let tx = TxEip1559 {
            chain_id: 2026,
            nonce: 7,
            gas_limit: 21000,
            max_fee_per_gas: 42,
            max_priority_fee_per_gas: 1,
            to: TxKind::Call(Address::repeat_byte(9)),
            value: U256::from(3),
            input: Bytes::from_static(b"OCOMP"),
            ..Default::default()
        };
        let signed = signer.sign_eip1559(tx).unwrap();
        assert_eq!(
            signed.hash().to_string(),
            "0x0c8ad4662e428cce0650561d5f26c3e35a7629d730ef5ea37d64692fefdcc919"
        );
        assert_eq!(
            signed.signature().to_string(),
            "0x5d337ec1c2a263805fd0e7103530a1001b913fbbb60245427649ec1fea3af9fc673acb064b85220c02d3bd213b63d7ef4e21d827562c03e4a283190223668ada1c"
        );
        assert_eq!(signed.try_recover().unwrap(), signer.address());
    }

    #[test]
    fn derives_expected_address_from_known_secret() {
        let signer = OutbeEvmSigner::from_hex(
            "0x0000000000000000000000000000000000000000000000000000000000000001",
        )
        .expect("valid signer");
        assert_eq!(
            signer.address(),
            address!("0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf")
        );
    }

    #[test]
    fn debug_redacts_secret() {
        let signer = OutbeEvmSigner::from_secret_bytes([1u8; 32]).expect("valid signer");
        let debug = format!("{signer:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&hex::encode([1u8; 32])));
    }

    #[test]
    fn signs_and_recovers_expected_address() {
        let signer = OutbeEvmSigner::from_secret_bytes([1u8; 32]).expect("valid signer");
        let input = SystemTxInputV2::CycleTick.encode().expect("input encodes");
        let tx = build_unsigned_system_tx(SystemTxKind::CycleTick, 0, 1, CHAIN_ID, input)
            .expect("system tx builds");

        let signed = signer.sign_unsigned(tx).expect("signs");
        let recovered = signed.try_recover().expect("recovers");
        assert_eq!(recovered, signer.address());
    }

    #[test]
    fn signatures_are_deterministic_for_same_unsigned_tx() {
        let signer = OutbeEvmSigner::from_secret_bytes([2u8; 32]).expect("valid signer");
        let input = SystemTxInputV2::CycleTick.encode().expect("input encodes");
        let tx_a = build_unsigned_system_tx(SystemTxKind::CycleTick, 0, 1, CHAIN_ID, input.clone())
            .expect("system tx builds");
        let tx_b = build_unsigned_system_tx(SystemTxKind::CycleTick, 0, 1, CHAIN_ID, input)
            .expect("system tx builds");

        let signed_a = signer.sign_unsigned(tx_a).expect("signs");
        let signed_b = signer.sign_unsigned(tx_b).expect("signs");
        assert_eq!(signed_a.signature(), signed_b.signature());
        assert_eq!(signed_a.hash(), signed_b.hash());
    }

    #[test]
    fn rejects_wrong_secret_length() {
        let err = OutbeEvmSigner::from_hex("0x1234").expect_err("length rejected");
        assert!(matches!(err, SignerError::InvalidSecretLength { len: 2 }));
    }

    #[test]
    fn default_key_path_is_sibling_to_bls_key() {
        let path = default_validator_evm_key_path(Path::new("/tmp/validator-1/signing-key.hex"));
        assert_eq!(path, PathBuf::from("/tmp/validator-1/evm-key.hex"));
    }

    #[test]
    fn signed_system_tx_keeps_reserved_to_and_zero_value() {
        let signer = OutbeEvmSigner::from_secret_bytes([3u8; 32]).expect("valid signer");
        let input = SystemTxInputV2::CycleTick.encode().expect("input encodes");
        let tx = build_unsigned_system_tx(SystemTxKind::CycleTick, 0, 1, CHAIN_ID, input)
            .expect("system tx builds");
        let signed = signer.sign_unsigned(tx).expect("signs");
        assert_eq!(signed.to(), Some(crate::system_tx::OUTBE_SYSTEM_TX_ADDRESS));
        assert_eq!(signed.value(), U256::ZERO);
        assert_eq!(
            signed.kind(),
            TxKind::Call(crate::system_tx::OUTBE_SYSTEM_TX_ADDRESS)
        );
        assert_eq!(signed.input(), &Bytes::from_static(b"OSC2\x02"));
    }
}
