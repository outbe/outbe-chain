use std::ops::Deref;
use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::crypto;

#[derive(Error, Debug)]
pub enum DidError {
    #[error("invalid did: {0}")]
    Did(String),
    #[error("invalid public key: {0}")]
    PublicKey(#[from] crypto::repository::Error),
}

#[derive(Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
#[serde(into = "String", try_from = "String")]
pub struct Did(crypto::RepositoryKey);

impl Did {
    /// We use the format specified by the DID `key` method, which is described as:
    ///
    /// `did:key:MULTIBASE(base58-btc, MULTICODEC(public-key-type, raw-public-key-bytes))`
    ///
    pub fn encode(&self) -> String {
        format!("did:key:{}", self.0.to_human())
    }

    pub fn decode(input: &str) -> Result<Self, DidError> {
        let key = input
            .strip_prefix("did:key:")
            .ok_or_else(|| DidError::Did(input.to_owned()))?;

        crypto::RepositoryKey::from_str(key)
            .map(Did)
            .map_err(DidError::from)
    }

    pub fn as_key(&self) -> &crypto::RepositoryKey {
        self.deref()
    }

    pub fn as_node_id(&self) -> Option<&crypto::PublicKey> {
        self.0.as_ed25519()
    }
}

impl From<&crypto::PublicKey> for Did {
    fn from(key: &crypto::PublicKey) -> Self {
        Self((*key).into())
    }
}

impl From<crypto::PublicKey> for Did {
    fn from(key: crypto::PublicKey) -> Self {
        (&key).into()
    }
}

impl From<crypto::RepositoryKey> for Did {
    fn from(key: crypto::RepositoryKey) -> Self {
        Self(key)
    }
}

impl From<&crypto::RepositoryKey> for Did {
    fn from(key: &crypto::RepositoryKey) -> Self {
        Self(*key)
    }
}

impl From<Did> for crypto::RepositoryKey {
    fn from(Did(key): Did) -> Self {
        key
    }
}

impl From<&Did> for crypto::RepositoryKey {
    fn from(did: &Did) -> Self {
        did.0
    }
}

impl TryFrom<Did> for crypto::PublicKey {
    type Error = crypto::repository::Error;

    fn try_from(did: Did) -> Result<Self, Self::Error> {
        did.as_node_id()
            .copied()
            .ok_or(crypto::repository::Error::UnsupportedKey)
    }
}

impl From<Did> for String {
    fn from(other: Did) -> Self {
        other.encode()
    }
}

impl FromStr for Did {
    type Err = DidError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::decode(s)
    }
}

impl TryFrom<String> for Did {
    type Error = DidError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::decode(&value)
    }
}

impl fmt::Display for Did {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.encode())
    }
}

impl fmt::Debug for Did {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Did({:?})", self.to_string())
    }
}

impl Deref for Did {
    type Target = crypto::RepositoryKey;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod test {
    use super::*;

    #[test]
    fn test_did_encode_decode() {
        let input = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
        let Did(key) = Did::decode(input).unwrap();

        assert_eq!(Did::from(key).encode(), input);
    }

    #[test]
    fn bls_did_is_repository_authority_only() {
        let key = "zUC76MTqiMm7Ka3fDpoTkRQL8PieYCgweoLJKEZTk8tNhchUmQse8UnRJX3fDZaNsjqUwxnKrq1D27Dg5rU6hZf27Yq1GAVKhyHGUhUFs799KRUzgmUTgP4vYjkDHvPWTZraK1h";
        let input = format!("did:key:{key}");
        let did = Did::decode(&input).unwrap();
        assert_eq!(did.encode(), input);
        assert!(did.as_node_id().is_none());
        assert!(crypto::PublicKey::try_from(did).is_err());
        assert!(key.parse::<crypto::PublicKey>().is_err());
        let encoded = serde_json::to_string(&did).unwrap();
        assert_eq!(serde_json::from_str::<Did>(&encoded).unwrap(), did);
        let namespace = did.as_key().to_namespace();
        assert_eq!(namespace.as_str(), format!("refs/namespaces/{key}"));

        let ed = Did::decode("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK").unwrap();
        assert_eq!(
            crypto::PublicKey::try_from(ed).unwrap(),
            *ed.as_node_id().unwrap()
        );
    }

    #[test]
    fn test_did_vectors() {
        Did::decode("did:key:z6MkiTBz1ymuepAQ4HEHYSF1H8quG5GLVVQR3djdX3mDooWp").unwrap();
        Did::decode("did:key:z6MkjchhfUsD6mmvni8mCdXHw216Xrm9bQe2mBH1P5RDjVJG").unwrap();
        Did::decode("did:key:z6MknGc3ocHs3zdPiJbnaaqDi58NGb4pk1Sp9WxWufuXSdxf").unwrap();
    }
}
