//! Canonical fingerprint of an equivocation evidence pair.

use alloy_primitives::{keccak256, B256};

/// The supported same-signer consensus equivocation classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EquivocationEvidenceKind {
    /// Two notarizations of different proposals in one round.
    ConflictingNotarize,
    /// Two finalizations of different proposals in one round.
    ConflictingFinalize,
    /// A nullification and finalization in one round.
    NullifyFinalize,
}

/// Computes the order-independent hash of two equivocation evidence blocks.
///
/// The wire-frozen preimage is the lexicographically ordered raw concatenation
/// `first || second`, with no length prefix or separator. Changing it would
/// change the on-chain evidence deduplication keys.
///
/// Callers pass blocks validated by `EvidenceBlock::parse` to the
/// `pubkey[48] || sig[96] || proposal` layout: at least 145 bytes per block,
/// hence at least 290 bytes per pair, disjoint from the 62-byte epoch-felony key.
/// This helper does not validate blocks or frame arbitrary byte-slice boundaries.
pub fn canonical_equivocation_evidence_hash(ev1: &[u8], ev2: &[u8]) -> B256 {
    let (first, second) = if ev1 <= ev2 { (ev1, ev2) } else { (ev2, ev1) };
    let mut buf = Vec::with_capacity(first.len() + second.len());
    buf.extend_from_slice(first);
    buf.extend_from_slice(second);
    keccak256(&buf)
}

#[cfg(test)]
mod tests {
    use super::canonical_equivocation_evidence_hash;
    use alloy_primitives::{b256, keccak256};

    #[test]
    fn hash_is_order_independent() {
        let a = [0x01; 145];
        let b = [0x02; 146];
        assert_eq!(
            canonical_equivocation_evidence_hash(&a, &b),
            canonical_equivocation_evidence_hash(&b, &a),
        );
    }

    #[test]
    fn hash_matches_pinned_vector() {
        let a = [0x01; 145];
        let b = [0x02; 145];
        let expected = b256!("0f5af8afac742ef6abecd1c6eb8db597870166234131d009ea4d703da777ccf3");
        assert_eq!(canonical_equivocation_evidence_hash(&a, &b), expected);
        assert_eq!(canonical_equivocation_evidence_hash(&b, &a), expected);
    }

    #[test]
    fn hash_orders_lexicographically_not_by_length() {
        let a = [0x02];
        let b = [0x01, 0xff];
        assert_eq!(
            canonical_equivocation_evidence_hash(&a, &b),
            keccak256([0x01, 0xff, 0x02]),
        );
        let a = [0x01];
        let b = [0x01, 0x00];
        assert_eq!(
            canonical_equivocation_evidence_hash(&a, &b),
            keccak256([0x01, 0x01, 0x00]),
        );
    }

    #[test]
    fn identical_blocks_hash_as_doubled_preimage() {
        let a = [0x03; 145];
        assert_eq!(
            canonical_equivocation_evidence_hash(&a, &a),
            keccak256([0x03; 290]),
        );
    }

    #[test]
    fn distinct_fixed_header_pairs_do_not_collide() {
        let block = |proposal| {
            let mut bytes = [0x01; 145];
            bytes[144] = proposal;
            bytes
        };
        let a = block(0x10);
        let b = block(0x20);
        let c = block(0x30);
        let ab = canonical_equivocation_evidence_hash(&a, &b);
        let ac = canonical_equivocation_evidence_hash(&a, &c);
        let bc = canonical_equivocation_evidence_hash(&b, &c);
        assert_ne!(ab, ac);
        assert_ne!(ab, bc);
        assert_ne!(ac, bc);
    }
}
