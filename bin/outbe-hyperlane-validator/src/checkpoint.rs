//! Hyperlane checkpoint and announcement types, signing hashes and JSON.
//!
//! Ported from hyperlane-monorepo `agents-v3.0.0`, `rust/main/hyperlane-core`:
//! `types/checkpoint.rs`, `types/announcement.rs`, `traits/signing.rs`,
//! `utils.rs` (Apache-2.0, see `LICENSE-APACHE`). Changes: alloy types, only
//! the types that the validator writes.
//!
//! The JSON must stay byte-compatible with the upstream agent: the relayer
//! reads `checkpoint_<index>_with_id.json` from the bucket.

use alloy_primitives::{eip191_hash_message, keccak256, Address, Signature, B256, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

/// A value that a Hyperlane validator signs with EIP-191.
pub trait Signable {
    /// The 32-byte hash. The validator signs its EIP-191 form.
    fn signing_hash(&self) -> B256;
}

/// A Hyperlane checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub merkle_tree_hook_address: B256,
    pub mailbox_domain: u32,
    pub root: B256,
    pub index: u32,
}

/// A checkpoint and the id of the message at `checkpoint.index`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointWithMessageId {
    pub checkpoint: Checkpoint,
    pub message_id: B256,
}

impl Signable for CheckpointWithMessageId {
    /// `keccak(domain_hash(hook, domain) || root || index_be32 || message_id)`.
    fn signing_hash(&self) -> B256 {
        let c = &self.checkpoint;
        let mut buf = Vec::with_capacity(32 + 32 + 4 + 32);
        buf.extend_from_slice(
            domain_hash(c.merkle_tree_hook_address, c.mailbox_domain, b"HYPERLANE").as_slice(),
        );
        buf.extend_from_slice(c.root.as_slice());
        buf.extend_from_slice(&c.index.to_be_bytes());
        buf.extend_from_slice(self.message_id.as_slice());
        keccak256(buf)
    }
}

/// The announcement of a validator's checkpoint storage location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    #[serde(serialize_with = "lowercase_address")]
    pub validator: Address,
    pub mailbox_address: B256,
    pub mailbox_domain: u32,
    pub storage_location: String,
}

impl Signable for Announcement {
    /// `keccak(domain_hash(mailbox, domain) || storage_location)`.
    fn signing_hash(&self) -> B256 {
        let mut buf = Vec::with_capacity(32 + self.storage_location.len());
        buf.extend_from_slice(
            domain_hash(
                self.mailbox_address,
                self.mailbox_domain,
                b"HYPERLANE_ANNOUNCEMENT",
            )
            .as_slice(),
        );
        buf.extend_from_slice(self.storage_location.as_bytes());
        keccak256(buf)
    }
}

/// `keccak(domain_be32 || address || tag)`.
fn domain_hash(address: B256, domain: u32, tag: &[u8]) -> B256 {
    let mut buf = Vec::with_capacity(4 + 32 + tag.len());
    buf.extend_from_slice(&domain.to_be_bytes());
    buf.extend_from_slice(address.as_slice());
    buf.extend_from_slice(tag);
    keccak256(buf)
}

/// Upstream serializes `H160` as lowercase hex. Alloy uses the checksum form.
fn lowercase_address<S: Serializer>(address: &Address, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{address:#x}"))
}

/// The upstream ECDSA signature JSON: `{"r": "0x…", "s": "0x…", "v": 27}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SignatureJson {
    r: U256,
    s: U256,
    v: u64,
}

/// A value and the validator's signature over it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed<T> {
    pub value: T,
    pub signature: Signature,
}

impl<T: Signable> Signed<T> {
    /// Signs `value` with EIP-191, as `ethers` `Signer::sign_message` does.
    pub fn sign(value: T, signer: &PrivateKeySigner) -> alloy_signer::Result<Self> {
        let signature = signer.sign_hash_sync(&eip191_hash_message(value.signing_hash()))?;
        Ok(Self { value, signature })
    }

    /// Returns the address that signed `value`.
    pub fn recover(&self) -> Result<Address, alloy_primitives::SignatureError> {
        self.signature
            .recover_address_from_prehash(&eip191_hash_message(self.value.signing_hash()))
    }
}

impl<T> Signed<T> {
    /// The 65-byte `r || s || v` form with `v` in {27, 28}. Contracts and
    /// the `serialized_signature` JSON field use this form.
    pub fn signature_bytes(&self) -> [u8; 65] {
        self.signature.as_bytes()
    }
}

impl<T: Serialize> Serialize for Signed<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let sig = SignatureJson {
            r: self.signature.r(),
            s: self.signature.s(),
            v: 27 + u64::from(self.signature.v()),
        };
        let mut state = s.serialize_struct("SignedType", 3)?;
        state.serialize_field("value", &self.value)?;
        state.serialize_field("signature", &sig)?;
        state.serialize_field(
            "serialized_signature",
            &format!(
                "0x{}",
                alloy_primitives::hex::encode(self.signature_bytes())
            ),
        )?;
        state.end()
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Signed<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw<T> {
            #[serde(alias = "checkpoint", alias = "announcement")]
            value: T,
            signature: SignatureJson,
        }
        let raw = Raw::<T>::deserialize(d)?;
        let parity = match raw.signature.v {
            27 | 0 => false,
            28 | 1 => true,
            v => return Err(serde::de::Error::custom(format!("invalid signature v {v}"))),
        };
        Ok(Self {
            value: raw.value,
            signature: Signature::new(raw.signature.r, raw.signature.s, parity),
        })
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256, Bytes};
    use serde_json::Value;

    use super::*;

    fn vector() -> Value {
        serde_json::from_str(include_str!("../testdata/outbe-testnet-checkpoints.json")).unwrap()
    }

    fn signed<T>(value: T, signature: &Value) -> Signed<T> {
        let bytes: Bytes = serde_json::from_value(signature.clone()).unwrap();
        Signed {
            value,
            signature: Signature::try_from(bytes.as_ref()).unwrap(),
        }
    }

    /// Upstream `vectors/domainHash.json`, first case.
    #[test]
    fn domain_hash_matches_upstream_vector() {
        assert_eq!(
            domain_hash(
                b256!("0x0000000000000000000000002222222222222222222222222222222222222222"),
                1,
                b"HYPERLANE",
            ),
            b256!("0xbbca56eb98960a4637eb40486d9a069550dd70d9c185ed138516e8e33cf3d7e7")
        );
    }

    /// Checkpoints that the upstream agent signed and the controller accepted.
    #[test]
    fn testnet_checkpoint_signatures_recover_to_signer() {
        let v = vector();
        let hook: Address = serde_json::from_value(v["merkle_tree_hook"].clone()).unwrap();
        let domain = v["domain"].as_u64().unwrap() as u32;
        for c in v["checkpoints"].as_array().unwrap() {
            let value = CheckpointWithMessageId {
                checkpoint: Checkpoint {
                    merkle_tree_hook_address: hook.into_word(),
                    mailbox_domain: domain,
                    root: serde_json::from_value(c["root"].clone()).unwrap(),
                    index: c["index"].as_u64().unwrap() as u32,
                },
                message_id: serde_json::from_value(c["message_id"].clone()).unwrap(),
            };
            let signer: Address = serde_json::from_value(c["signer"].clone()).unwrap();
            let signed = signed(value, &c["signature"]);
            assert_eq!(signed.recover().unwrap(), signer);
            let raw: Bytes = serde_json::from_value(c["signature"].clone()).unwrap();
            assert_eq!(signed.signature_bytes().as_slice(), raw.as_ref());
        }
    }

    /// The announcement that the upstream agent sent to `ValidatorAnnounce`.
    #[test]
    fn testnet_announcement_signature_recovers_to_validator() {
        let v = vector();
        let a = &v["announcement"];
        let mailbox: Address = serde_json::from_value(v["mailbox"].clone()).unwrap();
        let value = Announcement {
            validator: serde_json::from_value(a["validator"].clone()).unwrap(),
            mailbox_address: mailbox.into_word(),
            mailbox_domain: v["domain"].as_u64().unwrap() as u32,
            storage_location: a["storage_location"].as_str().unwrap().to_owned(),
        };
        let validator = value.validator;
        assert_eq!(signed(value, &a["signature"]).recover().unwrap(), validator);
    }

    /// The exact bytes that upstream `serde_json::to_vec` writes, with a
    /// low `r` to check the minimal `U256` hex form.
    #[test]
    fn checkpoint_json_matches_upstream_bytes() {
        let signed = Signed {
            value: CheckpointWithMessageId {
                checkpoint: Checkpoint {
                    merkle_tree_hook_address: address!(
                        "0x1f4DB1C0405361288843CD4B0861A8D687554708"
                    )
                    .into_word(),
                    mailbox_domain: 54322345,
                    root: B256::repeat_byte(0xab),
                    index: 7,
                },
                message_id: B256::repeat_byte(0xcd),
            },
            signature: Signature::new(U256::from(0x1a2b_u64), U256::from(1_u64), true),
        };
        let expected = concat!(
            r#"{"value":{"checkpoint":{"merkle_tree_hook_address":"#,
            r#""0x0000000000000000000000001f4db1c0405361288843cd4b0861a8d687554708","#,
            r#""mailbox_domain":54322345,"#,
            r#""root":"0xabababababababababababababababababababababababababababababababab","#,
            r#""index":7},"#,
            r#""message_id":"0xcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"},"#,
            r#""signature":{"r":"0x1a2b","s":"0x1","v":28},"#,
            r#""serialized_signature":"0x"#,
            "0000000000000000000000000000000000000000000000000000000000001a2b",
            "0000000000000000000000000000000000000000000000000000000000000001",
            r#"1c"}"#,
        );
        let json = serde_json::to_string(&signed).unwrap();
        assert_eq!(json, expected);
        let back: Signed<CheckpointWithMessageId> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, signed);
    }

    /// A checkpoint file that the upstream agent wrote on localnet. Upstream
    /// writes it with `serde_json::to_vec`, so the compact form must match.
    #[test]
    fn agent_checkpoint_file_round_trips_byte_exact() {
        let agent = concat!(
            r#"{"value":{"checkpoint":{"merkle_tree_hook_address":"#,
            r#""0x0000000000000000000000006543cef9bbe42d66b5b36ccaf9374de2b55f9cbc","#,
            r#""mailbox_domain":54322345,"#,
            r#""root":"0x5cd1ceeff6f033960677157f13b472fcfb1617638dbdfb0ab0cde36cd94bf050","#,
            r#""index":2},"#,
            r#""message_id":"0x3eae2775d2a39169fd82693c9883351a72761c9cdf4cddedaf95bf62e454250e"},"#,
            r#""signature":{"r":"0x82079df9c6de4d2ab039216e3275cf803d245819150512e50dde105e2802f989","#,
            r#""s":"0x76b8041d134256ef7df29fa8a8613a5c042af46c3468ab011410e19e6e1cb493","v":28},"#,
            r#""serialized_signature":"0x82079df9c6de4d2ab039216e3275cf803d245819150512e50dde105e2802f989"#,
            r#"76b8041d134256ef7df29fa8a8613a5c042af46c3468ab011410e19e6e1cb4931c"}"#,
        );
        let signed: Signed<CheckpointWithMessageId> = serde_json::from_str(agent).unwrap();
        assert_eq!(serde_json::to_string(&signed).unwrap(), agent);
    }

    #[test]
    fn announcement_json_uses_lowercase_validator() {
        let signer = PrivateKeySigner::random();
        let value = Announcement {
            validator: address!("0xBf663D6C0f5dA824Fe7857a78E58b4E53Ec53af5"),
            mailbox_address: B256::ZERO,
            mailbox_domain: 1,
            storage_location: "s3+http://h:9000/b/1".to_owned(),
        };
        let json = serde_json::to_string(&Signed::sign(value, &signer).unwrap()).unwrap();
        assert!(json.contains(r#""validator":"0xbf663d6c0f5da824fe7857a78e58b4e53ec53af5""#));
    }

    #[test]
    fn sign_then_recover() {
        let signer = PrivateKeySigner::random();
        let value = CheckpointWithMessageId {
            checkpoint: Checkpoint {
                merkle_tree_hook_address: B256::repeat_byte(1),
                mailbox_domain: 2,
                root: B256::repeat_byte(3),
                index: 4,
            },
            message_id: B256::repeat_byte(5),
        };
        let signed = Signed::sign(value, &signer).unwrap();
        assert_eq!(signed.recover().unwrap(), signer.address());
        assert!(matches!(signed.signature_bytes()[64], 27 | 28));
    }
}
