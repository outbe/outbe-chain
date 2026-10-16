use alloc::{boxed::Box, vec::Vec};

use super::{BlsEnvelope, ENVELOPE_LENGTH, Error, RepositoryKey, RepositorySubject};
use crate::{Signature, VerifyingKey, signature::Verifier as _};

/// A native repository signature. Decoding alone does not authorize an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepositorySignature {
    Ed25519(Signature),
    Bls12381G2(Box<BlsEnvelope>),
}

impl From<Signature> for RepositorySignature {
    fn from(signature: Signature) -> Self {
        Self::Ed25519(signature)
    }
}

impl From<BlsEnvelope> for RepositorySignature {
    fn from(envelope: BlsEnvelope) -> Self {
        Self::Bls12381G2(Box::new(envelope))
    }
}

impl RepositorySignature {
    /// Signed-refs signature blobs keep the legacy 64-byte Ed25519 representation.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        match bytes.len() {
            64 => Signature::from_slice(bytes)
                .map(Self::Ed25519)
                .map_err(|_| Error::InvalidSignature),
            ENVELOPE_LENGTH => BlsEnvelope::from_bytes(bytes).map(Self::from),
            n => Err(Error::Length(n)),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Ed25519(signature) => signature.to_bytes().to_vec(),
            Self::Bls12381G2(envelope) => envelope.to_bytes().to_vec(),
        }
    }

    /// `repo`, `context` and `blob` must come from the verifier's actual repository/job.
    pub fn verify_identity(
        &self,
        authority: RepositoryKey,
        repo: [u8; 20],
        context: [u8; 32],
        blob: [u8; 20],
    ) -> Result<(), Error> {
        self.verify(authority, &blob, || {
            Ok(RepositorySubject::identity(repo, context, blob))
        })
    }

    /// Verify the actual tree and exact unsigned commit, including its trailers.
    pub fn verify_cob(
        &self,
        authority: RepositoryKey,
        repo: [u8; 20],
        context: [u8; 32],
        tree: [u8; 20],
        unsigned_commit: &[u8],
    ) -> Result<(), Error> {
        self.verify(authority, &tree, || {
            Ok(RepositorySubject::cob(repo, context, tree, unsigned_commit))
        })
    }

    /// Verify actual canonical refs and the actual previous signed-refs commit.
    pub fn verify_signed_refs(
        &self,
        authority: RepositoryKey,
        repo: [u8; 20],
        context: [u8; 32],
        previous: [u8; 20],
        canonical_refs: &[u8],
    ) -> Result<(), Error> {
        self.verify(authority, canonical_refs, || {
            RepositorySubject::signed_refs(repo, context, previous, canonical_refs)
        })
    }

    fn verify(
        &self,
        authority: RepositoryKey,
        legacy_payload: &[u8],
        subject: impl FnOnce() -> Result<RepositorySubject, Error>,
    ) -> Result<(), Error> {
        match (self, authority) {
            (Self::Ed25519(signature), RepositoryKey::Ed25519(key)) => VerifyingKey::try_from(&key)
                .map_err(|_| Error::InvalidKey)?
                .verify(legacy_payload, signature)
                .map_err(|_| Error::InvalidSignature),
            (Self::Bls12381G2(envelope), RepositoryKey::Bls12381G2(_)) => {
                envelope.verify(&subject()?, authority)
            }
            _ => Err(Error::SubjectMismatch),
        }
    }
}

#[cfg(all(feature = "serde", feature = "multibase"))]
mod serde_impl {
    use alloc::string::String;
    use core::fmt;

    use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeStruct as _};

    use super::*;

    const ALGORITHM: &str = "bls12381-g2-v1";
    const FIELDS: &[&str] = &["algorithm", "envelope"];

    impl Serialize for RepositorySignature {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            match self {
                Self::Ed25519(signature) => serializer.serialize_str(&multibase::encode(
                    multibase::Base::Base58Btc,
                    signature.to_bytes(),
                )),
                Self::Bls12381G2(envelope) => {
                    let mut object = serializer.serialize_struct("RepositorySignature", 2)?;
                    object.serialize_field("algorithm", ALGORITHM)?;
                    object.serialize_field("envelope", &envelope.to_hex())?;
                    object.end()
                }
            }
        }
    }

    impl<'de> Deserialize<'de> for RepositorySignature {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Visitor;

            impl<'de> de::Visitor<'de> for Visitor {
                type Value = RepositorySignature;

                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("a legacy Ed25519 multibase string or a BLS signature object")
                }

                fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                    let (_, bytes) = multibase::decode(value).map_err(E::custom)?;
                    Signature::from_slice(&bytes)
                        .map(RepositorySignature::Ed25519)
                        .map_err(E::custom)
                }

                fn visit_map<M: de::MapAccess<'de>>(
                    self,
                    mut map: M,
                ) -> Result<Self::Value, M::Error> {
                    let mut algorithm = None;
                    let mut envelope = None;
                    while let Some(field) = map.next_key::<String>()? {
                        match field.as_str() {
                            "algorithm" => {
                                if algorithm.is_some() {
                                    return Err(de::Error::duplicate_field("algorithm"));
                                }
                                algorithm = Some(map.next_value::<String>()?);
                            }
                            "envelope" => {
                                if envelope.is_some() {
                                    return Err(de::Error::duplicate_field("envelope"));
                                }
                                envelope = Some(map.next_value::<String>()?);
                            }
                            _ => return Err(de::Error::unknown_field(&field, FIELDS)),
                        }
                    }
                    let algorithm =
                        algorithm.ok_or_else(|| de::Error::missing_field("algorithm"))?;
                    if algorithm != ALGORITHM {
                        return Err(de::Error::custom(
                            "unsupported repository signature algorithm",
                        ));
                    }
                    let envelope = envelope.ok_or_else(|| de::Error::missing_field("envelope"))?;
                    BlsEnvelope::from_hex(&envelope)
                        .map(RepositorySignature::from)
                        .map_err(de::Error::custom)
                }
            }

            deserializer.deserialize_any(Visitor)
        }
    }
}

#[cfg(all(test, feature = "multibase"))]
mod tests {
    use super::*;
    use crate::{SigningKey, signature::Signer as _};
    use alloc::vec;

    fn bytes(input: &str) -> Vec<u8> {
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn native_verifiers_reconstruct_each_actual_subject() {
        let v: serde_json::Value = serde_json::from_str(include_str!("vectors.json")).unwrap();
        let key: RepositoryKey = v["key"].as_str().unwrap().parse().unwrap();
        let repo = bytes(v["repo"].as_str().unwrap()).try_into().unwrap();
        for row in v["vectors"].as_array().unwrap() {
            let s = RepositorySignature::from(
                BlsEnvelope::from_hex(row["envelope"].as_str().unwrap()).unwrap(),
            );
            let native = bytes(row["native"].as_str().unwrap());
            let context = bytes(row["context"].as_str().unwrap()).try_into().unwrap();
            let tree = native.get(..20).unwrap().try_into().unwrap();
            let unsigned = bytes(v["unsigned_commit"].as_str().unwrap());
            let previous = bytes(v["previous_sigrefs"].as_str().unwrap())
                .try_into()
                .unwrap();
            let verify = |repo, context, key| match row["kind"].as_u64().unwrap() {
                1 => s.verify_identity(key, repo, context, tree),
                2 => s.verify_cob(key, repo, context, tree, &unsigned),
                3 => s.verify_signed_refs(key, repo, context, previous, &native),
                _ => unreachable!(),
            };
            assert_eq!(verify(repo, context, key), Ok(()));
            assert!(verify([0; 20], context, key).is_err());
            assert!(verify(repo, [0; 32], key).is_err());
            assert!(verify(repo, context, crate::PublicKey::from_bytes([0; 32]).into()).is_err());
            assert!(s.verify_identity(key, repo, context, [0; 20]).is_err());
            assert!(
                s.verify_cob(key, repo, context, [0; 20], &unsigned)
                    .is_err()
            );
            assert!(
                s.verify_cob(key, repo, context, tree, b"altered trailers\n")
                    .is_err()
            );
            assert!(
                s.verify_signed_refs(key, repo, context, [0; 20], &native)
                    .is_err()
            );
            assert!(
                s.verify_signed_refs(key, repo, context, previous, b"altered refs")
                    .is_err()
            );
            let encoded = s.to_bytes();
            assert_eq!(RepositorySignature::from_bytes(&encoded).unwrap(), s);
            assert!(RepositorySignature::from_bytes(&encoded[..268]).is_err());
            assert!(RepositorySignature::from_bytes(&[encoded, vec![0]].concat()).is_err());
        }
    }

    #[test]
    fn legacy_native_signatures_keep_original_bytes_and_subjects() {
        let signer = SigningKey::mock(42);
        let key = signer.public_key().into();
        let oid = [0x41; 20];
        let s: RepositorySignature = signer.sign(&oid).into();
        assert_eq!(s.to_bytes(), signer.sign(&oid).to_bytes());
        assert_eq!(RepositorySignature::from_bytes(&s.to_bytes()).unwrap(), s);
        assert_eq!(s.verify_identity(key, [0; 20], [0; 32], oid), Ok(()));
        assert_eq!(
            s.verify_cob(key, [0; 20], [0; 32], oid, b"legacy metadata"),
            Ok(())
        );
        assert!(s.verify_identity(key, [0; 20], [0; 32], [0; 20]).is_err());
        let refs = b"legacy canonical refs";
        let s: RepositorySignature = signer.sign(refs).into();
        assert_eq!(
            s.verify_signed_refs(key, [0; 20], [0; 32], [0; 20], refs),
            Ok(())
        );
        assert!(
            s.verify_signed_refs(key, [0; 20], [0; 32], [0; 20], b"other refs")
                .is_err()
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn json_preserves_legacy_and_rejects_ambiguous_bls_objects() {
        use alloc::{format, string::String};
        let signer = SigningKey::mock(42);
        let raw = signer.sign(b"legacy");
        let legacy = RepositorySignature::from(raw);
        let expected = format!(
            "\"{}\"",
            multibase::encode(multibase::Base::Base58Btc, raw.to_bytes())
        );
        assert_eq!(serde_json::to_string(&legacy).unwrap(), expected);
        assert_eq!(
            serde_json::from_str::<RepositorySignature>(&expected).unwrap(),
            legacy
        );
        let v: serde_json::Value = serde_json::from_str(include_str!("vectors.json")).unwrap();
        let hex = v["vectors"][0]["envelope"].as_str().unwrap();
        let valid = format!(r#"{{"algorithm":"bls12381-g2-v1","envelope":"{hex}"}}"#);
        let s: RepositorySignature = serde_json::from_str(&valid).unwrap();
        assert_eq!(serde_json::to_string(&s).unwrap(), valid);
        let reverse = format!(r#"{{"envelope":"{hex}","algorithm":"bls12381-g2-v1"}}"#);
        assert_eq!(
            serde_json::from_str::<RepositorySignature>(&reverse).unwrap(),
            s
        );
        for invalid in [
            valid.replace("bls12381-g2-v1", "ed25519"),
            valid.replace("bls12381-g2-v1", "bls12381-g2-v2"),
            valid.replace("{", r#"{"algorithm":"bls12381-g2-v1","#),
            valid.replace("{", &format!(r#"{{"envelope":"{hex}","#)),
            valid.replace("{", r#"{"extra":true,"#),
            valid.replace(hex, &hex.to_uppercase()),
            valid.replace(hex, &hex[..536]),
            valid.replace(hex, &(String::from(hex) + "00")),
            format!("\"{hex}\""),
            format!(r#"{{"envelope":"{hex}"}}"#),
            String::from(r#"{"algorithm":"bls12381-g2-v1"}"#),
            String::from(r#"{"algorithm":"bls12381-g2-v1","envelope":null}"#),
            String::from(r#"{"algorithm":1,"envelope":""}"#),
            String::from("[]"),
            String::from("null"),
            String::from("64"),
        ] {
            assert!(
                serde_json::from_str::<RepositorySignature>(&invalid).is_err(),
                "{invalid}"
            );
        }
        let mut invalid = s.to_bytes();
        invalid[..4].copy_from_slice(b"RDB2");
        assert!(RepositorySignature::from_bytes(&invalid).is_err());
    }
}
