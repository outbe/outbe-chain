//! Repository authority keys and detached BLS signatures. Transport keys stay Ed25519.

mod signature;
pub use signature::RepositorySignature;

#[cfg(feature = "multibase")]
use alloc::string::String;
#[cfg(all(
    feature = "multibase",
    any(feature = "radicle-git-ref-format", feature = "sqlite")
))]
use alloc::string::ToString as _;
use alloc::vec::Vec;
use blst::{BLST_ERROR, min_sig};
use sha2::{Digest as _, Sha256};

use crate::PublicKey;

const DOMAIN: &[u8] = b"radicle.repository.bls.v1";
const DST: &[u8] = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_POP_";
const MESSAGE_LENGTH: usize = 125;
pub const ENVELOPE_LENGTH: usize = 269;
pub const MAX_NATIVE_PAYLOAD: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid repository encoding length {0}")]
    Length(usize),
    #[error("unsupported repository key codec or algorithm")]
    UnsupportedKey,
    #[error("invalid or noncanonical repository key")]
    InvalidKey,
    #[error("invalid BLS signature")]
    InvalidSignature,
    #[error("invalid repository signing subject")]
    InvalidSubject,
    #[error("BLS signature does not match the actual repository subject or authority")]
    SubjectMismatch,
    #[error("native payload exceeds the repository signing limit")]
    PayloadTooLarge,
}

/// Validated canonical compressed G2 point; never a transport/Noise key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlsPublicKey([u8; 96]);

impl BlsPublicKey {
    pub fn from_bytes(bytes: [u8; 96]) -> Result<Self, Error> {
        let key = min_sig::PublicKey::key_validate(&bytes).map_err(|_| Error::InvalidKey)?;
        if key.to_bytes() != bytes {
            return Err(Error::InvalidKey);
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 96] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(
    all(feature = "serde", feature = "multibase"),
    derive(serde::Serialize, serde::Deserialize),
    serde(into = "String", try_from = "String")
)]
#[cfg_attr(
    all(feature = "schemars", feature = "serde", feature = "multibase"),
    derive(schemars::JsonSchema)
)]
pub enum RepositoryKey {
    Ed25519(PublicKey),
    Bls12381G2(BlsPublicKey),
}

#[cfg(feature = "qcheck")]
impl qcheck::Arbitrary for RepositoryKey {
    fn arbitrary(g: &mut qcheck::Gen) -> Self {
        if bool::arbitrary(g) {
            return PublicKey::arbitrary(g).into();
        }
        let mut seed = [0; 32];
        for byte in &mut seed {
            *byte = u8::arbitrary(g);
        }
        let key = min_sig::SecretKey::key_gen(&seed, &[])
            .expect("a 32-byte seed is sufficient for BLS key generation");
        BlsPublicKey::from_bytes(key.sk_to_pk().to_bytes())
            .expect("generated BLS keys are valid subgroup points")
            .into()
    }
}

impl From<&RepositoryKey> for RepositoryKey {
    fn from(key: &RepositoryKey) -> Self {
        *key
    }
}

impl From<PublicKey> for RepositoryKey {
    fn from(key: PublicKey) -> Self {
        Self::Ed25519(key)
    }
}

impl From<&PublicKey> for RepositoryKey {
    fn from(key: &PublicKey) -> Self {
        Self::from(*key)
    }
}

impl From<BlsPublicKey> for RepositoryKey {
    fn from(key: BlsPublicKey) -> Self {
        Self::Bls12381G2(key)
    }
}

impl TryFrom<&RepositoryKey> for crate::VerifyingKey {
    type Error = crate::signature::Error;

    fn try_from(key: &RepositoryKey) -> Result<Self, Self::Error> {
        Self::try_from(key.as_ed25519().ok_or_else(crate::signature::Error::new)?)
    }
}

impl RepositoryKey {
    /// A repository authority need not identify any transport peer.
    pub fn as_ed25519(&self) -> Option<&PublicKey> {
        match self {
            Self::Ed25519(key) => Some(key),
            Self::Bls12381G2(_) => None,
        }
    }

    pub fn to_tagged_bytes(&self) -> Vec<u8> {
        match self {
            Self::Ed25519(key) => [b"\0".as_slice(), key.into_inner().as_slice()].concat(),
            Self::Bls12381G2(key) => [b"\x01".as_slice(), key.as_bytes()].concat(),
        }
    }

    /// Decode one complete V2 repository-key field, rejecting trailing bytes.
    pub fn from_tagged_bytes(bytes: &[u8]) -> Result<Self, Error> {
        match bytes.first() {
            Some(0) if bytes.len() == 33 => {
                let mut key = [0; 32];
                key.copy_from_slice(&bytes[1..]);
                let key = PublicKey::from_bytes(key);
                crate::VerifyingKey::try_from(&key).map_err(|_| Error::InvalidKey)?;
                Ok(Self::Ed25519(key))
            }
            Some(1) if bytes.len() == 97 => {
                let mut key = [0; 96];
                key.copy_from_slice(&bytes[1..]);
                BlsPublicKey::from_bytes(key).map(Self::Bls12381G2)
            }
            Some(0 | 1) | None => Err(Error::Length(bytes.len())),
            Some(_) => Err(Error::UnsupportedKey),
        }
    }

    #[cfg(feature = "multibase")]
    pub fn to_human(&self) -> String {
        match self {
            Self::Ed25519(key) => key.to_human(),
            Self::Bls12381G2(key) => {
                let mut bytes = [0; 98];
                bytes[..2].copy_from_slice(&[0xeb, 0x01]);
                bytes[2..].copy_from_slice(key.as_bytes());
                multibase::encode(multibase::Base::Base58Btc, bytes)
            }
        }
    }

    #[cfg(all(feature = "radicle-git-ref-format", feature = "multibase"))]
    pub fn to_namespace(&self) -> radicle_git_ref_format::RefString {
        use alloc::borrow::ToOwned as _;
        use radicle_git_ref_format::name::{NAMESPACES, REFS};
        REFS.to_owned()
            .and(NAMESPACES)
            .and(radicle_git_ref_format::Component::from(self))
    }
}

#[cfg(all(feature = "sqlite", feature = "multibase"))]
impl TryFrom<&sqlite::Value> for RepositoryKey {
    type Error = sqlite::Error;

    fn try_from(value: &sqlite::Value) -> Result<Self, Self::Error> {
        let parse = match value {
            sqlite::Value::String(s) => s.parse().map_err(|e: Error| e.to_string()),
            _ => Err(String::from("sql: invalid type for repository key")),
        };
        parse.map_err(|message| sqlite::Error {
            code: None,
            message: Some(message),
        })
    }
}

#[cfg(all(feature = "sqlite", feature = "multibase"))]
impl sqlite::BindableWithIndex for &RepositoryKey {
    fn bind<I: sqlite::ParameterIndex>(
        self,
        statement: &mut sqlite::Statement<'_>,
        index: I,
    ) -> sqlite::Result<()> {
        sqlite::Value::from(self).bind(statement, index)
    }
}

#[cfg(all(feature = "sqlite", feature = "multibase"))]
impl From<&RepositoryKey> for sqlite::Value {
    fn from(key: &RepositoryKey) -> Self {
        Self::String(key.to_human())
    }
}

#[cfg(feature = "multibase")]
impl core::str::FromStr for RepositoryKey {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (base, bytes) = multibase::decode(input).map_err(|_| Error::InvalidKey)?;
        match bytes.get(..2) {
            Some([0xed, 0x01]) => input
                .parse::<PublicKey>()
                .map(Self::Ed25519)
                .map_err(|_| Error::InvalidKey),
            Some([0xeb, 0x01]) if bytes.len() == 98 && base == multibase::Base::Base58Btc => {
                let mut key = [0; 96];
                key.copy_from_slice(&bytes[2..]);
                let key = Self::Bls12381G2(BlsPublicKey::from_bytes(key)?);
                if key.to_human() != input {
                    return Err(Error::InvalidKey);
                }
                Ok(key)
            }
            Some([0xeb, 0x01]) => Err(Error::InvalidKey),
            _ => Err(Error::UnsupportedKey),
        }
    }
}

#[cfg(feature = "multibase")]
impl core::fmt::Display for RepositoryKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.to_human().fmt(f)
    }
}

#[cfg(feature = "multibase")]
impl From<RepositoryKey> for String {
    fn from(key: RepositoryKey) -> Self {
        key.to_human()
    }
}

#[cfg(feature = "multibase")]
impl TryFrom<String> for RepositoryKey {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

#[cfg(all(feature = "radicle-git-ref-format", feature = "multibase"))]
impl From<&RepositoryKey> for radicle_git_ref_format::Component<'_> {
    fn from(key: &RepositoryKey) -> Self {
        use radicle_git_ref_format::{Component, RefString};
        let name = RefString::try_from(key.to_string())
            .expect("canonical repository keys are valid ref strings");
        Component::from_refstr(name).expect("canonical repository keys are valid ref components")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SubjectKind {
    Identity = 1,
    Cob = 2,
    SignedRefs = 3,
}

/// Reconstruct this from actual native bytes and trusted context before verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositorySubject([u8; MESSAGE_LENGTH]);

impl RepositorySubject {
    fn new(
        kind: SubjectKind,
        repo: [u8; 20],
        context: [u8; 32],
        metadata: [u8; 32],
        payload: &[u8],
    ) -> Result<Self, Error> {
        if payload.len() > MAX_NATIVE_PAYLOAD {
            return Err(Error::PayloadTooLarge);
        }
        if kind != SubjectKind::SignedRefs && payload.len() != 20 {
            return Err(Error::InvalidSubject);
        }
        let mut bytes = [0; MESSAGE_LENGTH];
        bytes[..4].copy_from_slice(b"RDB1");
        bytes[4] = kind as u8;
        bytes[5..25].copy_from_slice(&repo);
        bytes[25..57].copy_from_slice(&context);
        bytes[57..89].copy_from_slice(&metadata);
        bytes[89..93].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes[93..].copy_from_slice(&Sha256::digest(payload));
        Ok(Self(bytes))
    }

    pub fn identity(repo: [u8; 20], context: [u8; 32], blob: [u8; 20]) -> Self {
        Self::new(SubjectKind::Identity, repo, context, [0; 32], &blob)
            .expect("identity OID always has the required fixed length")
    }

    pub fn cob(repo: [u8; 20], context: [u8; 32], tree: [u8; 20], unsigned_commit: &[u8]) -> Self {
        Self::new(
            SubjectKind::Cob,
            repo,
            context,
            Sha256::digest(unsigned_commit).into(),
            &tree,
        )
        .expect("COB tree OID always has the required fixed length")
    }

    pub fn signed_refs(
        repo: [u8; 20],
        context: [u8; 32],
        previous: [u8; 20],
        canonical_refs: &[u8],
    ) -> Result<Self, Error> {
        Self::new(
            SubjectKind::SignedRefs,
            repo,
            context,
            Sha256::digest(previous).into(),
            canonical_refs,
        )
    }

    pub fn as_bytes(&self) -> &[u8; MESSAGE_LENGTH] {
        &self.0
    }

    fn decode(bytes: [u8; MESSAGE_LENGTH]) -> Result<Self, Error> {
        if &bytes[..4] != b"RDB1" || !matches!(bytes[4], 1..=3) {
            return Err(Error::InvalidSubject);
        }
        let length = u32::from_be_bytes([bytes[89], bytes[90], bytes[91], bytes[92]]) as usize;
        if length > MAX_NATIVE_PAYLOAD {
            return Err(Error::PayloadTooLarge);
        }
        if (bytes[4] != 3 && length != 20) || (bytes[4] == 1 && bytes[57..89] != [0; 32]) {
            return Err(Error::InvalidSubject);
        }
        Ok(Self(bytes))
    }
}

/// A structural envelope is not authority: verify against a separately reconstructed subject/key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlsEnvelope {
    subject: RepositorySubject,
    key: BlsPublicKey,
    signature: [u8; 48],
}

impl BlsEnvelope {
    /// Attach an externally produced (including recovered threshold) signature.
    pub fn attach(
        subject: RepositorySubject,
        key: BlsPublicKey,
        signature: [u8; 48],
    ) -> Result<Self, Error> {
        let envelope = Self {
            subject,
            key,
            signature,
        };
        Self::from_bytes(&envelope.to_bytes())?;
        envelope.verify(&envelope.subject, key.into())?;
        Ok(envelope)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != ENVELOPE_LENGTH {
            return Err(Error::Length(bytes.len()));
        }
        let mut subject = [0; MESSAGE_LENGTH];
        subject.copy_from_slice(&bytes[..MESSAGE_LENGTH]);
        let mut key = [0; 96];
        key.copy_from_slice(&bytes[MESSAGE_LENGTH..MESSAGE_LENGTH + 96]);
        let mut signature = [0; 48];
        signature.copy_from_slice(&bytes[MESSAGE_LENGTH + 96..]);
        let sig = min_sig::Signature::sig_validate(&signature, true)
            .map_err(|_| Error::InvalidSignature)?;
        if sig.to_bytes() != signature {
            return Err(Error::InvalidSignature);
        }
        Ok(Self {
            subject: RepositorySubject::decode(subject)?,
            key: BlsPublicKey::from_bytes(key)?,
            signature,
        })
    }

    pub fn to_bytes(&self) -> [u8; ENVELOPE_LENGTH] {
        let mut bytes = [0; ENVELOPE_LENGTH];
        bytes[..MESSAGE_LENGTH].copy_from_slice(self.subject.as_bytes());
        bytes[MESSAGE_LENGTH..MESSAGE_LENGTH + 96].copy_from_slice(self.key.as_bytes());
        bytes[MESSAGE_LENGTH + 96..].copy_from_slice(&self.signature);
        bytes
    }

    pub fn key(&self) -> BlsPublicKey {
        self.key
    }

    pub fn to_hex(&self) -> alloc::string::String {
        use core::fmt::Write as _;
        let mut encoded = alloc::string::String::with_capacity(ENVELOPE_LENGTH * 2);
        for byte in self.to_bytes() {
            write!(encoded, "{byte:02x}").expect("writing to a String is infallible");
        }
        encoded
    }

    pub fn from_hex(input: &str) -> Result<Self, Error> {
        if input.len() != ENVELOPE_LENGTH * 2 {
            return Err(Error::Length(input.len()));
        }
        fn digit(byte: u8) -> Result<u8, Error> {
            match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                _ => Err(Error::InvalidSignature),
            }
        }
        let mut bytes = [0; ENVELOPE_LENGTH];
        for (out, pair) in bytes.iter_mut().zip(input.as_bytes().chunks_exact(2)) {
            *out = (digit(pair[0])? << 4) | digit(pair[1])?;
        }
        Self::from_bytes(&bytes)
    }

    pub fn verify(
        &self,
        actual: &RepositorySubject,
        authority: RepositoryKey,
    ) -> Result<(), Error> {
        if self.subject != *actual || authority != RepositoryKey::from(self.key) {
            return Err(Error::SubjectMismatch);
        }
        let key =
            min_sig::PublicKey::key_validate(self.key.as_bytes()).map_err(|_| Error::InvalidKey)?;
        let sig = min_sig::Signature::sig_validate(&self.signature, true)
            .map_err(|_| Error::InvalidSignature)?;
        // Commonware union_unique: canonical varint namespace length, namespace, message.
        let mut message = Vec::with_capacity(1 + DOMAIN.len() + MESSAGE_LENGTH);
        message.push(DOMAIN.len() as u8);
        message.extend_from_slice(DOMAIN);
        message.extend_from_slice(actual.as_bytes());
        if sig.verify(true, &message, DST, &[], &key, true) != BLST_ERROR::BLST_SUCCESS {
            return Err(Error::InvalidSignature);
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "multibase"))]
mod tests {
    use super::*;
    use crate::SigningKey;
    #[cfg(not(any(feature = "radicle-git-ref-format", feature = "sqlite")))]
    use alloc::string::ToString as _;
    use alloc::vec;

    fn bytes(input: &str) -> Vec<u8> {
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    fn vectors() -> serde_json::Value {
        serde_json::from_str(include_str!("repository/vectors.json")).unwrap()
    }

    #[test]
    fn commonware_threshold_vectors_and_subjects() {
        let vectors = vectors();
        let authority: RepositoryKey = vectors["key"].as_str().unwrap().parse().unwrap();
        let repo: [u8; 20] = bytes(vectors["repo"].as_str().unwrap()).try_into().unwrap();
        for row in vectors["vectors"].as_array().unwrap() {
            let native = bytes(row["native"].as_str().unwrap());
            let context: [u8; 32] = bytes(row["context"].as_str().unwrap()).try_into().unwrap();
            let subject = match row["kind"].as_u64().unwrap() {
                1 => RepositorySubject::identity(
                    repo,
                    context,
                    native.as_slice().try_into().unwrap(),
                ),
                2 => RepositorySubject::cob(
                    repo,
                    context,
                    native.as_slice().try_into().unwrap(),
                    &bytes(vectors["unsigned_commit"].as_str().unwrap()),
                ),
                3 => RepositorySubject::signed_refs(
                    repo,
                    context,
                    bytes(vectors["previous_sigrefs"].as_str().unwrap())
                        .try_into()
                        .unwrap(),
                    &native,
                )
                .unwrap(),
                _ => unreachable!(),
            };
            assert_eq!(
                subject.as_bytes().as_slice(),
                bytes(row["message"].as_str().unwrap())
            );
            let input = row["envelope"].as_str().unwrap();
            let envelope = BlsEnvelope::from_hex(input).unwrap();
            envelope.verify(&subject, authority).unwrap();
            assert_eq!(envelope.to_hex(), input);
            let raw = envelope.to_bytes();
            assert_eq!(BlsEnvelope::from_bytes(&raw).unwrap(), envelope);
            let signature = raw[221..].try_into().unwrap();
            assert_eq!(
                BlsEnvelope::attach(subject.clone(), envelope.key(), signature).unwrap(),
                envelope
            );

            for index in [4, 5, 25, 57, 89, 93] {
                let mut altered = subject.clone();
                altered.0[index] ^= 1;
                assert!(
                    envelope.verify(&altered, authority).is_err(),
                    "subject byte {index}"
                );
            }
            let ed: RepositoryKey = SigningKey::mock(17).public_key().into();
            assert_eq!(envelope.verify(&subject, ed), Err(Error::SubjectMismatch));
            let mut bad = raw;
            bad[268] ^= 1;
            assert!(
                BlsEnvelope::from_bytes(&bad)
                    .and_then(|x| x.verify(&subject, authority))
                    .is_err()
            );
            assert!(BlsEnvelope::from_hex(&input.to_uppercase()).is_err());
            assert!(BlsEnvelope::from_bytes(&raw[..268]).is_err());
            assert!(BlsEnvelope::from_bytes(&[raw.as_slice(), b"\0"].concat()).is_err());
        }
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn repository_key_sql_roundtrip_preserves_algorithm() {
        let db = sqlite::open(":memory:").unwrap();
        db.execute("CREATE TABLE authorities (key TEXT NOT NULL)")
            .unwrap();
        let ed = RepositoryKey::from(SigningKey::mock(7).public_key());
        let bls: RepositoryKey = vectors()["key"].as_str().unwrap().parse().unwrap();
        for key in [ed, bls] {
            let mut insert = db.prepare("INSERT INTO authorities VALUES (?)").unwrap();
            insert.bind((1, &key)).unwrap();
            assert_eq!(insert.next().unwrap(), sqlite::State::Done);
        }
        let mut query = db
            .prepare("SELECT key FROM authorities ORDER BY rowid")
            .unwrap();
        for key in [ed, bls] {
            assert_eq!(query.next().unwrap(), sqlite::State::Row);
            let value = query.read::<sqlite::Value, _>(0).unwrap();
            assert_eq!(RepositoryKey::try_from(&value).unwrap(), key);
            assert_eq!(crate::VerifyingKey::try_from(&key).is_ok(), key == ed);
        }
        assert_eq!(query.next().unwrap(), sqlite::State::Done);
        assert!(RepositoryKey::try_from(&sqlite::Value::Integer(1)).is_err());
        assert!(RepositoryKey::try_from(&sqlite::Value::String(String::from("invalid"))).is_err());
    }

    #[test]
    fn canonical_repository_keys_preserve_ed25519() {
        let ed = *SigningKey::mock(7).public_key();
        let key = RepositoryKey::from(ed);
        assert_eq!(key.to_string(), ed.to_string());
        assert_eq!(key.to_string().parse::<RepositoryKey>().unwrap(), key);
        assert_eq!(
            RepositoryKey::from_tagged_bytes(&key.to_tagged_bytes()).unwrap(),
            key
        );
        assert_eq!(key.as_ed25519(), Some(&ed));
        let vectors = vectors();
        let input = vectors["key"].as_str().unwrap();
        let key: RepositoryKey = input.parse().unwrap();
        assert_eq!(key.to_string(), input);
        assert!(key.as_ed25519().is_none());
        assert_eq!(
            RepositoryKey::from_tagged_bytes(&key.to_tagged_bytes()).unwrap(),
            key
        );
        for bytes in [
            vec![],
            vec![2; 97],
            vec![1; 96],
            [key.to_tagged_bytes(), vec![0]].concat(),
        ] {
            assert!(RepositoryKey::from_tagged_bytes(&bytes).is_err());
        }
        let (_, raw) = multibase::decode(input).unwrap();
        assert!(
            multibase::encode(multibase::Base::Base16Lower, &raw)
                .parse::<RepositoryKey>()
                .is_err()
        );
        let noncanonical_codec = [&[0xeb, 0x81, 0x00][..], &raw[2..]].concat();
        assert!(
            multibase::encode(multibase::Base::Base58Btc, noncanonical_codec)
                .parse::<RepositoryKey>()
                .is_err()
        );
        #[cfg(feature = "serde")]
        for key in [RepositoryKey::from(ed), key] {
            let encoded = serde_json::to_string(&key).unwrap();
            assert_eq!(
                serde_json::from_str::<RepositoryKey>(&encoded).unwrap(),
                key
            );
            assert_eq!(encoded, serde_json::to_string(&key.to_human()).unwrap());
        }
    }

    #[test]
    fn cross_domain_and_valid_wrong_signatures_are_rejected() {
        let vectors = vectors();
        let mut raw = bytes(vectors["vectors"][0]["envelope"].as_str().unwrap());
        let other = bytes(vectors["vectors"][1]["envelope"].as_str().unwrap());
        raw[221..].copy_from_slice(&other[221..]);
        let wrong = BlsEnvelope::from_bytes(&raw).unwrap();
        assert_eq!(
            wrong.verify(&wrong.subject, wrong.key().into()),
            Err(Error::InvalidSignature)
        );

        let secret = min_sig::SecretKey::key_gen(&[42; 32], &[]).unwrap();
        let key = BlsPublicKey::from_bytes(secret.sk_to_pk().to_bytes()).unwrap();
        let subject = RepositorySubject::identity([1; 20], [2; 32], [3; 20]);
        let framed = |domain: &[u8]| {
            [
                alloc::vec![domain.len() as u8],
                domain.to_vec(),
                subject.as_bytes().to_vec(),
            ]
            .concat()
        };
        let valid = secret.sign(&framed(DOMAIN), DST, &[]).to_bytes();
        BlsEnvelope::attach(subject.clone(), key, valid).unwrap();
        for message in [
            subject.as_bytes().to_vec(),
            framed(b"another.repository.domain"),
        ] {
            let signature = secret.sign(&message, DST, &[]).to_bytes();
            assert_eq!(
                BlsEnvelope::attach(subject.clone(), key, signature),
                Err(Error::InvalidSignature)
            );
        }
        let signature = secret
            .sign(
                &framed(DOMAIN),
                b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_",
                &[],
            )
            .to_bytes();
        assert_eq!(
            BlsEnvelope::attach(subject, key, signature),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn invalid_curve_subgroup_and_identity_points_are_rejected() {
        assert!(BlsPublicKey::from_bytes([0; 96]).is_err());
        let mut infinity = [0; 96];
        infinity[0] = 0xc0;
        assert!(BlsPublicKey::from_bytes(infinity).is_err());
        let mut found_wrong_subgroup = false;
        for x in 0_u32..100 {
            let mut encoded = [0; 96];
            encoded[0] = 0x80;
            encoded[92..].copy_from_slice(&x.to_be_bytes());
            if let Ok(point) = min_sig::PublicKey::from_bytes(&encoded)
                && point.validate() == Err(BLST_ERROR::BLST_POINT_NOT_IN_GROUP)
            {
                assert!(BlsPublicKey::from_bytes(encoded).is_err());
                found_wrong_subgroup = true;
                break;
            }
        }
        assert!(found_wrong_subgroup);
        let vectors = vectors();
        let mut raw = bytes(vectors["vectors"][0]["envelope"].as_str().unwrap());
        for first in [0x00, 0x80, 0xc0] {
            raw[221..].fill(0);
            raw[221] = first;
            assert!(BlsEnvelope::from_bytes(&raw).is_err());
        }
    }

    #[test]
    fn malformed_subjects_and_native_bounds_are_rejected() {
        let vectors = vectors();
        let raw = bytes(vectors["vectors"][0]["envelope"].as_str().unwrap());
        for (offset, value) in [(0, b'X'), (4, 0), (4, 4), (57, 1), (92, 19)] {
            let mut bad = raw.clone();
            bad[offset] = value;
            assert!(BlsEnvelope::from_bytes(&bad).is_err());
        }
        let mut bad = raw;
        bad[89..93].copy_from_slice(&(MAX_NATIVE_PAYLOAD as u32 + 1).to_be_bytes());
        assert_eq!(BlsEnvelope::from_bytes(&bad), Err(Error::PayloadTooLarge));
        assert!(
            RepositorySubject::signed_refs([0; 20], [0; 32], [0; 20], &vec![0; MAX_NATIVE_PAYLOAD])
                .is_ok()
        );
        assert_eq!(
            RepositorySubject::signed_refs(
                [0; 20],
                [0; 32],
                [0; 20],
                &vec![0; MAX_NATIVE_PAYLOAD + 1]
            ),
            Err(Error::PayloadTooLarge)
        );
    }
}
