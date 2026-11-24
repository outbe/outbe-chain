//! Native BLS12-381 MinSig verification over EIP-2537 wire encodings.
//!
//! Public keys live in G2 (256-byte EIP-2537 layout: `x.c0 || x.c1 || y.c0 || y.c1`,
//! each 48-byte field element left-padded with 16 zero bytes).
//! Signatures live in G1 (128-byte EIP-2537 layout: `x || y`, each 48-byte
//! field element left-padded with 16 zero bytes).
//! Messages are hashed to G1 per RFC 9380 (`expand_message_xmd` SHA-256 + SSWU + cofactor clear).

use blst::{min_sig, BLST_ERROR};

/// Byte length of an EIP-2537 uncompressed G1 point (`x || y`, 64 bytes each).
pub const EIP2537_G1_LEN: usize = 128;

/// Byte length of an EIP-2537 uncompressed G2 point (`x.c0 || x.c1 || y.c0 || y.c1`, 64 bytes each).
pub const EIP2537_G2_LEN: usize = 256;

/// Decodes and subgroup-validates a canonical, non-identity EIP-2537 G2 public key.
pub fn decode_eip2537_g2(pk_att: &[u8]) -> Option<min_sig::PublicKey> {
    if pk_att.len() != EIP2537_G2_LEN {
        return None;
    }
    if pk_att.iter().all(|&b| b == 0) {
        return None;
    }
    // blst uncompressed G2 ordering is x.c1 || x.c0 || y.c1 || y.c0 (48 bytes each).
    let mut uncompressed = [0u8; 192];
    for (dst_idx, src_idx) in [1usize, 0, 3, 2].into_iter().enumerate() {
        let limb = &pk_att[src_idx * 64..(src_idx + 1) * 64];
        if limb[..16] != [0u8; 16] || (limb[16] & 0xe0) != 0 {
            return None;
        }
        uncompressed[dst_idx * 48..(dst_idx + 1) * 48].copy_from_slice(&limb[16..]);
    }
    let pk = min_sig::PublicKey::deserialize(&uncompressed).ok()?;
    if pk.validate().is_err() {
        return None;
    }
    Some(pk)
}

/// Decodes and subgroup-validates a canonical EIP-2537 G1 signature point.
pub fn decode_eip2537_g1(sig_g1: &[u8]) -> Option<min_sig::Signature> {
    if sig_g1.len() != EIP2537_G1_LEN {
        return None;
    }
    if sig_g1.iter().all(|&b| b == 0) {
        // Point at infinity in EIP-2537; cannot satisfy pairing for a non-identity pk
        // against a random-oracle hash-to-curve point, and blst rejects infinity sigs.
        return None;
    }
    let mut uncompressed = [0u8; 96];
    for limb_idx in 0..2 {
        let limb = &sig_g1[limb_idx * 64..(limb_idx + 1) * 64];
        if limb[..16] != [0u8; 16] || (limb[16] & 0xe0) != 0 {
            return None;
        }
        uncompressed[limb_idx * 48..(limb_idx + 1) * 48].copy_from_slice(&limb[16..]);
    }
    let sig = min_sig::Signature::deserialize(&uncompressed).ok()?;
    if sig.validate(false).is_err() {
        return None;
    }
    Some(sig)
}

/// Encodes a `blst::min_sig::PublicKey` into the 256-byte EIP-2537 G2 wire layout.
pub fn encode_eip2537_g2(pk: &min_sig::PublicKey) -> [u8; EIP2537_G2_LEN] {
    let serialized = pk.serialize();
    let mut out = [0u8; EIP2537_G2_LEN];
    for (dst_idx, src_idx) in [1usize, 0, 3, 2].into_iter().enumerate() {
        out[dst_idx * 64 + 16..(dst_idx + 1) * 64]
            .copy_from_slice(&serialized[src_idx * 48..(src_idx + 1) * 48]);
    }
    out
}

/// Encodes a `blst::min_sig::Signature` into the 128-byte EIP-2537 G1 wire layout.
pub fn encode_eip2537_g1(sig: &min_sig::Signature) -> [u8; EIP2537_G1_LEN] {
    let serialized = sig.serialize();
    let mut out = [0u8; EIP2537_G1_LEN];
    out[16..64].copy_from_slice(&serialized[0..48]);
    out[80..128].copy_from_slice(&serialized[48..96]);
    out
}

/// Verifies a BLS12-381 MinSig signature (`sig_g1` in G1, `pk_g2` in G2) over `message`
/// hashed to G1 under RFC 9380 domain separation tag `dst`.
pub fn verify_eip2537_min_sig(pk_g2: &[u8], message: &[u8], sig_g1: &[u8], dst: &[u8]) -> bool {
    if dst.is_empty() || dst.len() > 255 {
        return false;
    }
    let Some(pk) = decode_eip2537_g2(pk_g2) else {
        return false;
    };
    let Some(sig) = decode_eip2537_g1(sig_g1) else {
        return false;
    };
    sig.verify(true, message, dst, &[], &pk, true) == BLST_ERROR::BLST_SUCCESS
}
