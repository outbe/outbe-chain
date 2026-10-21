//! Evidence parsing and verification for Simplex equivocation proofs.
//!
//! Verifies BLS MinPk signatures against the Simplex signed-payload format:
//!   `union_unique(namespace, proposal_bytes)`
//! where `union_unique` is `leb128(len(namespace)) || namespace || message`.

use alloy_primitives::B256;
use commonware_cryptography::bls12381;
use commonware_utils::ordered::Set;
use outbe_primitives::error::{PrecompileError, Result};

use crate::errors::SlashIndicatorError;

/// The ordered committee (BLS MinPk public keys) that ran the evidence's epoch. The
/// vote namespaces are committee-bound. Evidence verification must therefore use
/// the SAME committee that the Simplex signer used. The runtime supplies it from the
/// epoch's on-chain `CommitteeSnapshot`. The namespaces thus bind both the chain
/// (`outbe_app_namespace()`) and the committee. As a result, a vote from another chain
/// OR another committee can no longer be replayed here as fabricated double-sign
/// evidence. The committee-bound namespaces come from `outbe_consensus::proof`. That
/// module is the single source of truth, and the signer uses it too.
pub(crate) type EvidenceCommittee = Set<bls12381::PublicKey>;

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
            return Err(SlashIndicatorError::EvidenceBlockTooShort.into());
        }
        // The length guard above proves that these fixed slices have the exact size,
        // so the conversions cannot fail. Map to a structured error instead of
        // `unwrap()` on the precompile path.
        let pubkey: [u8; 48] = data[..48]
            .try_into()
            .map_err(|_| PrecompileError::from(SlashIndicatorError::EvidencePubkeySlice))?;
        let signature: [u8; 96] = data[48..144]
            .try_into()
            .map_err(|_| PrecompileError::from(SlashIndicatorError::EvidenceSignatureSlice))?;
        let proposal_bytes = data[144..].to_vec();
        Ok(Self {
            pubkey,
            signature,
            proposal_bytes,
        })
    }

    /// Verifies the BLS MinPk signature against the Simplex notarize payload,
    /// committee-bound to `committee`.
    pub fn verify_notarize_signature(&self, committee: &EvidenceCommittee) -> Result<()> {
        self.verify_bls_signature(&outbe_consensus::proof::notarize_namespace(committee))
    }

    /// Verifies the BLS MinPk signature against the Simplex nullify payload,
    /// committee-bound to `committee`.
    pub fn verify_nullify_signature(&self, committee: &EvidenceCommittee) -> Result<()> {
        self.verify_bls_signature(&outbe_consensus::proof::nullify_namespace(committee))
    }

    /// Verifies the BLS MinPk signature against the Simplex finalize payload,
    /// committee-bound to `committee`.
    pub fn verify_finalize_signature(&self, committee: &EvidenceCommittee) -> Result<()> {
        self.verify_bls_signature(&outbe_consensus::proof::finalize_namespace(committee))
    }

    /// Common BLS MinPk signature verification against a given namespace.
    fn verify_bls_signature(&self, namespace: &[u8]) -> Result<()> {
        use blst::min_pk::{PublicKey, Signature};
        use blst::BLST_ERROR;

        let pk = PublicKey::from_bytes(&self.pubkey)
            .map_err(|_| PrecompileError::from(SlashIndicatorError::InvalidBlsPublicKey))?;
        let sig = Signature::from_bytes(&self.signature)
            .map_err(|_| PrecompileError::from(SlashIndicatorError::InvalidBlsSignature))?;

        let signed_payload = build_signed_payload_with_ns(namespace, &self.proposal_bytes);

        // BLS MinPk verify with the CORRECT DST from commonware's bls12381 module.
        // Commonware uses POP_ (Proof of Possession) variant, not NUL_ (No Key Validation).
        let dst = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";
        let result = sig.verify(true, &signed_payload, dst, &[], &pk, true);
        if result != BLST_ERROR::BLST_SUCCESS {
            return Err(SlashIndicatorError::InvalidBlsSignature.into());
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

// Vote namespaces (notarize/nullify/finalize) come from
// `outbe_consensus::proof::*_namespace(committee)`. That is the single committee-bound
// derivation, and the signer uses it too. No local namespace builders remain,
// so the signer and the evidence verifier cannot drift.

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
            return Err(SlashIndicatorError::UnexpectedEndOfVarint.into());
        }
        let byte = data[*pos];
        *pos += 1;

        let payload = byte & 0x7F;
        // Only bit 63 remains in byte ten; reject excess bits before shifting.
        if shift == 63 && (payload > 1 || byte & 0x80 != 0) {
            return Err(SlashIndicatorError::VarintOverflow.into());
        }
        result |= u64::from(payload) << shift;
        if byte & 0x80 == 0 {
            if shift != 0 && payload == 0 {
                return Err(SlashIndicatorError::NonCanonicalVarint.into());
            }
            return Ok(result);
        }
        shift += 7;
        if shift >= 64 {
            return Err(SlashIndicatorError::VarintOverflow.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_signing::{self, signed_evidence, POP_DST};
    use blst::min_pk::{PublicKey, SecretKey};
    use commonware_codec::{varint::UInt, Encode as _};
    use commonware_cryptography::Signer as _;

    /// Signs `payload` under `ns` with the Simplex signer DST, then parses the
    /// evidence block with the production decoder.
    fn signed_block(sk: &SecretKey, pk: &PublicKey, ns: &[u8], payload: &[u8]) -> EvidenceBlock {
        EvidenceBlock::parse(&signed_evidence(sk, pk, ns, payload, POP_DST)).unwrap()
    }

    #[test]
    fn test_leb128_roundtrip() {
        for value in [0u64, 1, 127, 128, 255, 300, 16384, u64::MAX] {
            let mut buf = Vec::new();
            write_leb128(&mut buf, value);
            assert_eq!(
                buf,
                UInt(value).encode().to_vec(),
                "commonware varint {value}"
            );
            let mut pos = 0;
            let decoded = read_leb128(&buf, &mut pos).unwrap();
            assert_eq!(value, decoded, "failed for {value}");
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn leb128_rejects_noncanonical_overflowing_and_truncated_inputs() {
        for bytes in [
            vec![0x80, 0],
            vec![0x81, 0],
            vec![0xff, 0x80, 0],
            [vec![0x80; 9], vec![0]].concat(),
        ] {
            let err = read_leb128(&bytes, &mut 0).unwrap_err();
            assert_eq!(
                err.to_string(),
                PrecompileError::from(SlashIndicatorError::NonCanonicalVarint).to_string()
            );
        }
        for terminal in [2, 127, 128, 129, 255] {
            let bytes = [vec![0xff; 9], vec![terminal]].concat();
            let err = read_leb128(&bytes, &mut 0).unwrap_err();
            assert_eq!(
                err.to_string(),
                PrecompileError::from(SlashIndicatorError::VarintOverflow).to_string()
            );
        }
        for bytes in [vec![], vec![0x80], vec![0xff; 9]] {
            let err = read_leb128(&bytes, &mut 0).unwrap_err();
            assert_eq!(
                err.to_string(),
                PrecompileError::from(SlashIndicatorError::UnexpectedEndOfVarint).to_string()
            );
        }
    }

    #[test]
    fn leb128_preserves_cursor_and_subsequent_fields() {
        for value in [0, 1, 127, 128, 16384, u64::MAX] {
            let encoded = UInt(value).encode().to_vec();
            let mut data = vec![0xaa, 0xbb];
            data.extend_from_slice(&encoded);
            data.extend_from_slice(&[0xcc, 0xdd]);
            let mut pos = 2;
            let decoded = read_leb128(&data, &mut pos).unwrap();
            assert_eq!(decoded, value);
            assert_eq!(UInt(decoded).encode().as_ref(), &data[2..pos]);
            assert_eq!(&data[pos..], &[0xcc, 0xdd]);
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
    fn test_signed_payload_format() {
        // Verify the signed payload matches union_unique format:
        // leb128(len(ns)) || ns || proposal, where `ns` is the committee-bound
        // notarize namespace.
        let proposal = vec![1, 2, 3, 4];
        let ns = outbe_consensus::proof::notarize_namespace(&test_signing::committee());
        let payload = build_signed_payload_with_ns(&ns, &proposal);

        let nlen = ns.len();
        assert!(nlen < 128, "namespace fits a single leb128 byte");
        assert_eq!(payload[0], nlen as u8); // leb128(nlen)
        assert_eq!(&payload[1..1 + nlen], ns.as_slice());
        assert_eq!(&payload[1 + nlen..], &[1, 2, 3, 4]);
        assert_eq!(payload, commonware_utils::union_unique(&ns, &proposal));
    }

    #[test]
    fn test_verify_nullify_signature() {
        let (sk, pk) = test_signing::keypair(55).unwrap();

        // Nullify payload: epoch 5 + view 10, signed with the
        // committee-bound nullify namespace.
        let committee = test_signing::committee();
        let ns = outbe_consensus::proof::nullify_namespace(&committee);
        let block = signed_block(&sk, &pk, &ns, &test_signing::nullify_payload(5, 10));
        block.verify_nullify_signature(&committee).unwrap();

        // Notarize verification must fail for this signature
        assert!(block.verify_notarize_signature(&committee).is_err());
        // A different committee must also fail (committee binding).
        let other = Set::from_iter_dedup(
            (10u64..=13).map(|s| bls12381::PublicKey::from(bls12381::PrivateKey::from_seed(s))),
        );
        assert!(block.verify_nullify_signature(&other).is_err());

        let (epoch, view) = block.round().unwrap();
        assert_eq!(epoch, 5);
        assert_eq!(view, 10);
    }

    #[test]
    fn test_verify_finalize_signature() {
        let (sk, pk) = test_signing::keypair(77).unwrap();

        let committee = test_signing::committee();
        let ns = outbe_consensus::proof::finalize_namespace(&committee);
        // Chain-bound: no longer the bare b"outbe_FINALIZE".
        assert_ne!(ns.as_slice(), b"outbe_FINALIZE");
        let block = signed_block(&sk, &pk, &ns, &test_signing::proposal(4, 8, 7, [9u8; 32]));
        block.verify_finalize_signature(&committee).unwrap();
        // Cross-namespace must fail (domain separation).
        assert!(block.verify_notarize_signature(&committee).is_err());
        assert!(block.verify_nullify_signature(&committee).is_err());
    }

    #[test]
    fn test_verify_notarize_signature() {
        // Generate a BLS keypair
        let (sk, pk) = test_signing::keypair(42).unwrap();

        // A fake proposal (epoch=1, view=2, parent=0, digest=zeros), signed
        // with BLS under the committee-bound namespace.
        let committee = test_signing::committee();
        let ns = outbe_consensus::proof::notarize_namespace(&committee);
        let block = signed_block(&sk, &pk, &ns, &test_signing::proposal(1, 2, 0, [0u8; 32]));
        block.verify_notarize_signature(&committee).unwrap();

        let (epoch, view) = block.round().unwrap();
        assert_eq!(epoch, 1);
        assert_eq!(view, 2);
    }
}
