//! Evidence parsing and verification for Simplex equivocation proofs.
//!
//! Verifies BLS MinPk signatures against the Simplex signed-payload format:
//!   `union_unique(namespace, proposal_bytes)`
//! where `union_unique` is `leb128(len(namespace)) || namespace || message`.

use alloy_primitives::B256;
use outbe_primitives::error::{PrecompileError, Result};

/// The consensus namespace used by outbe's Simplex engine.
const CONSENSUS_NAMESPACE: &[u8] = b"outbe";
const NOTARIZE_SUFFIX: &[u8] = b"_NOTARIZE";
const NULLIFY_SUFFIX: &[u8] = b"_NULLIFY";

/// Parsed evidence block containing signer identity, signature, and proposal data.
pub(crate) struct EvidenceBlock {
    /// 48-byte BLS MinPk public key.
    pub pubkey: [u8; 48],
    /// 96-byte BLS MinPk signature.
    pub signature: [u8; 96],
    pub proposal_bytes: Vec<u8>,
}

impl EvidenceBlock {
    /// Parses an evidence block from raw bytes.
    ///
    /// Format: `pubkey[48] || signature[96] || proposal_bytes[remaining]`
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 48 + 96 + 1 {
            return Err(PrecompileError::Revert(
                "evidence block too short: need at least 145 bytes".into(),
            ));
        }
        let pubkey: [u8; 48] = data[..48].try_into().unwrap();
        let signature: [u8; 96] = data[48..144].try_into().unwrap();
        let proposal_bytes = data[144..].to_vec();
        Ok(Self {
            pubkey,
            signature,
            proposal_bytes,
        })
    }

    /// Verifies the BLS MinPk signature against the Simplex notarize payload format.
    pub fn verify_notarize_signature(&self) -> Result<()> {
        self.verify_bls_signature(&build_notarize_namespace())
    }

    /// Verifies the BLS MinPk signature against the Simplex nullify payload format.
    pub fn verify_nullify_signature(&self) -> Result<()> {
        self.verify_bls_signature(&build_nullify_namespace())
    }

    /// Common BLS MinPk signature verification against a given namespace.
    fn verify_bls_signature(&self, namespace: &[u8]) -> Result<()> {
        use blst::min_pk::{PublicKey, Signature};
        use blst::BLST_ERROR;

        let pk = PublicKey::from_bytes(&self.pubkey)
            .map_err(|_| PrecompileError::Revert("invalid BLS public key in evidence".into()))?;
        let sig = Signature::from_bytes(&self.signature)
            .map_err(|_| PrecompileError::Revert("invalid BLS signature in evidence".into()))?;

        let signed_payload = build_signed_payload_with_ns(namespace, &self.proposal_bytes);

        // A-02: BLS MinPk verify with the CORRECT DST from commonware's bls12381 module.
        // Commonware uses POP_ (Proof of Possession) variant, not NUL_ (No Key Validation).
        let dst = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";
        let result = sig.verify(true, &signed_payload, dst, &[], &pk, true);
        if result != BLST_ERROR::BLST_SUCCESS {
            return Err(PrecompileError::Revert(
                "invalid BLS signature in evidence".into(),
            ));
        }
        Ok(())
    }

    /// Extracts the round (epoch, view) from the encoded proposal bytes.
    ///
    /// Proposal encoding: `varint(epoch) || varint(view) || varint(parent) || digest[32]`
    pub fn round(&self) -> Result<(u64, u64)> {
        let mut pos = 0;
        let epoch = read_leb128(&self.proposal_bytes, &mut pos)?;
        let view = read_leb128(&self.proposal_bytes, &mut pos)?;
        Ok((epoch, view))
    }

    /// Returns the keccak256 hash of the signer's 48-byte BLS pubkey.
    ///
    /// Used for reverse lookup in the ValidatorSet contract.
    pub fn pubkey_hash(&self) -> B256 {
        alloy_primitives::keccak256(self.pubkey)
    }
}

/// Builds the full signed payload using a given namespace:
///   `leb128(len(namespace)) || namespace || payload_bytes`
fn build_signed_payload_with_ns(namespace: &[u8], payload_bytes: &[u8]) -> Vec<u8> {
    let ns_len = namespace.len();
    let mut buf = Vec::with_capacity(10 + ns_len + payload_bytes.len());
    write_leb128(&mut buf, ns_len as u64);
    buf.extend_from_slice(namespace);
    buf.extend_from_slice(payload_bytes);
    buf
}

/// Builds the notarize namespace: `b"outbe" || b"_NOTARIZE"`.
fn build_notarize_namespace() -> Vec<u8> {
    let mut ns = Vec::with_capacity(CONSENSUS_NAMESPACE.len() + NOTARIZE_SUFFIX.len());
    ns.extend_from_slice(CONSENSUS_NAMESPACE);
    ns.extend_from_slice(NOTARIZE_SUFFIX);
    ns
}

/// Builds the nullify namespace: `b"outbe" || b"_NULLIFY"`.
fn build_nullify_namespace() -> Vec<u8> {
    let mut ns = Vec::with_capacity(CONSENSUS_NAMESPACE.len() + NULLIFY_SUFFIX.len());
    ns.extend_from_slice(CONSENSUS_NAMESPACE);
    ns.extend_from_slice(NULLIFY_SUFFIX);
    ns
}

/// Encodes a u64 as LEB128 varint (matching commonware_codec::varint::UInt).
fn write_leb128(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            break;
        }
        buf.push(byte | 0x80);
    }
}

/// Reads a LEB128 varint from `data` starting at `pos`, advancing `pos`.
fn read_leb128(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        if *pos >= data.len() {
            return Err(PrecompileError::Revert(
                "unexpected end of data while reading varint".into(),
            ));
        }
        let byte = data[*pos];
        *pos += 1;

        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 64 {
            return Err(PrecompileError::Revert("varint overflow".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leb128_roundtrip() {
        for value in [0u64, 1, 127, 128, 255, 300, 16384, u64::MAX] {
            let mut buf = Vec::new();
            write_leb128(&mut buf, value);
            let mut pos = 0;
            let decoded = read_leb128(&buf, &mut pos).unwrap();
            assert_eq!(value, decoded, "failed for {value}");
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn test_evidence_block_parse() {
        let mut data = vec![0u8; 48 + 96 + 10]; // pubkey + sig + some proposal
        data[0] = 0xAA; // marker in pubkey
        data[48] = 0xBB; // marker in signature
        data[144] = 0xCC; // marker in proposal

        let block = EvidenceBlock::parse(&data).unwrap();
        assert_eq!(block.pubkey[0], 0xAA);
        assert_eq!(block.signature[0], 0xBB);
        assert_eq!(block.proposal_bytes[0], 0xCC);
    }

    #[test]
    fn test_evidence_block_too_short() {
        let data = vec![0u8; 144]; // exactly 144 = pubkey + sig, no proposal
        assert!(EvidenceBlock::parse(&data).is_err());
    }

    #[test]
    fn test_build_notarize_namespace() {
        let ns = build_notarize_namespace();
        assert_eq!(ns, b"outbe_NOTARIZE");
    }

    #[test]
    fn test_signed_payload_format() {
        // Verify the signed payload matches union_unique format
        let proposal = vec![1, 2, 3, 4];
        let ns = build_notarize_namespace();
        let payload = build_signed_payload_with_ns(&ns, &proposal);

        // namespace = "outbe_NOTARIZE" (14 bytes)
        // leb128(14) = 0x0E (single byte, 14 < 128)
        assert_eq!(payload[0], 14); // leb128(14)
        assert_eq!(&payload[1..15], b"outbe_NOTARIZE");
        assert_eq!(&payload[15..], &[1, 2, 3, 4]);
    }

    #[test]
    fn test_nullify_namespace() {
        let ns = build_nullify_namespace();
        assert_eq!(ns, b"outbe_NULLIFY");
    }

    #[test]
    fn test_verify_nullify_signature() {
        use blst::min_pk::SecretKey;

        let ikm = [55u8; 32];
        let sk = SecretKey::key_gen(&ikm, &[]).unwrap();
        let pk = sk.sk_to_pk();

        // Nullify payload: epoch + view (same varint encoding as proposal)
        let mut nullify_bytes = Vec::new();
        write_leb128(&mut nullify_bytes, 5); // epoch
        write_leb128(&mut nullify_bytes, 10); // view

        // Sign with nullify namespace
        let ns = build_nullify_namespace();
        let signed_payload = build_signed_payload_with_ns(&ns, &nullify_bytes);
        let dst = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";
        let sig = sk.sign(&signed_payload, dst, &[]);

        let mut data = Vec::new();
        data.extend_from_slice(&pk.to_bytes());
        data.extend_from_slice(&sig.to_bytes());
        data.extend_from_slice(&nullify_bytes);

        let block = EvidenceBlock::parse(&data).unwrap();
        block.verify_nullify_signature().unwrap();

        // Notarize verification must fail for this signature
        assert!(block.verify_notarize_signature().is_err());

        let (epoch, view) = block.round().unwrap();
        assert_eq!(epoch, 5);
        assert_eq!(view, 10);
    }

    #[test]
    fn test_verify_notarize_signature() {
        use blst::min_pk::SecretKey;

        // Generate a BLS keypair
        let ikm = [42u8; 32];
        let sk = SecretKey::key_gen(&ikm, &[]).unwrap();
        let pk = sk.sk_to_pk();

        // Create a fake proposal (epoch=1, view=2, parent=0, digest=zeros)
        let mut proposal_bytes = Vec::new();
        write_leb128(&mut proposal_bytes, 1); // epoch
        write_leb128(&mut proposal_bytes, 2); // view
        write_leb128(&mut proposal_bytes, 0); // parent
        proposal_bytes.extend_from_slice(&[0u8; 32]); // digest

        // Sign the full payload with BLS
        let ns = build_notarize_namespace();
        let signed_payload = build_signed_payload_with_ns(&ns, &proposal_bytes);
        let dst = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";
        let sig = sk.sign(&signed_payload, dst, &[]);

        // Build evidence block
        let mut data = Vec::new();
        data.extend_from_slice(&pk.to_bytes());
        data.extend_from_slice(&sig.to_bytes());
        data.extend_from_slice(&proposal_bytes);

        let block = EvidenceBlock::parse(&data).unwrap();
        block.verify_notarize_signature().unwrap();

        let (epoch, view) = block.round().unwrap();
        assert_eq!(epoch, 1);
        assert_eq!(view, 2);
    }
}
