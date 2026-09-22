//! Creator authentication over the original manifest bytes, independently of data validity.

use std::io;

use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::manifest_digest;

const SCHEME: &str = "secp256k1-recoverable-low-s-sha256";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureEnvelope {
    pub version: u32,
    pub scheme: String,
    pub bundle_id: String,
    pub public_key: String,
    pub signature: String,
}

pub fn signing_digest(raw_manifest: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"outbe/snapshot/manifest/v1\0");
    digest.update(manifest_digest(raw_manifest));
    digest.finalize().into()
}

impl SignatureEnvelope {
    /// The caller owns key loading and signing. Only a valid signature is accepted.
    pub fn from_signature(raw_manifest: &[u8], signature: [u8; 65]) -> io::Result<Self> {
        let public_key = recover(&signing_digest(raw_manifest), &signature)?;
        Ok(Self {
            version: 1,
            scheme: SCHEME.to_owned(),
            bundle_id: hex::encode(manifest_digest(raw_manifest)),
            public_key: hex::encode(public_key),
            signature: hex::encode(signature),
        })
    }

    pub fn from_bytes(raw: &[u8]) -> io::Result<Self> {
        serde_json::from_slice(raw).map_err(invalid)
    }

    /// Return the authenticated compressed key. Expected-key policy is operator supplied.
    pub fn verify(
        &self,
        raw_manifest: &[u8],
        expected_key: Option<&[u8; 33]>,
    ) -> io::Result<[u8; 33]> {
        if self.version != 1 || self.scheme != SCHEME {
            return Err(invalid("unsupported snapshot signature format"));
        }
        if decode::<32>(&self.bundle_id)? != manifest_digest(raw_manifest) {
            return Err(invalid("signed manifest digest differs"));
        }
        let declared = decode::<33>(&self.public_key)?;
        let recovered = recover(
            &signing_digest(raw_manifest),
            &decode::<65>(&self.signature)?,
        )?;
        if declared != recovered {
            return Err(invalid(
                "snapshot signature does not match declared creator",
            ));
        }
        if expected_key.is_some_and(|key| key != &recovered) {
            return Err(invalid(
                "snapshot creator does not match expected public key",
            ));
        }
        Ok(recovered)
    }
}

fn recover(digest: &[u8; 32], bytes: &[u8; 65]) -> io::Result<[u8; 33]> {
    let signature = Signature::from_slice(&bytes[..64]).map_err(invalid)?;
    if signature.normalize_s().is_some() {
        return Err(invalid("snapshot signature must use low S"));
    }
    let recovery = RecoveryId::from_byte(bytes[64])
        .ok_or_else(|| invalid("invalid snapshot signature recovery ID"))?;
    let key = VerifyingKey::recover_from_prehash(digest, &signature, recovery).map_err(invalid)?;
    key.to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(invalid)
}

fn decode<const N: usize>(value: &str) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    hex::decode_to_slice(value, &mut bytes).map_err(invalid)?;
    Ok(bytes)
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
