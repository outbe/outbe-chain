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

impl EquivocationEvidenceKind {
    /// Stable operator-facing telemetry label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ConflictingNotarize => "conflicting_notarize",
            Self::ConflictingFinalize => "conflicting_finalize",
            Self::NullifyFinalize => "nullify_finalize",
        }
    }
}

/// Packs the exact on-chain evidence layout, without framing the signed payload.
pub fn pack_equivocation_evidence_block(
    pubkey: &[u8; 48],
    vote_sig: &[u8; 96],
    payload: &[u8],
) -> Vec<u8> {
    let mut block = Vec::with_capacity(144 + payload.len());
    block.extend_from_slice(pubkey);
    block.extend_from_slice(vote_sig);
    block.extend_from_slice(payload);
    block
}

/// Individually verified same-signer conflict, ready for execution-side submission.
#[derive(Clone, Debug)]
pub struct VerifiedEquivocationEvidence {
    pub kind: EquivocationEvidenceKind,
    pub epoch: u64,
    pub accused_pubkey: [u8; 48],
    pub first_block: Vec<u8>,
    pub second_block: Vec<u8>,
    pub evidence_hash: B256,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EquivocationPushOutcome {
    Enqueued,
    Duplicate,
    Full,
}

/// Bounded producer handoff. Duplicate conflicts never replace pending evidence.
#[derive(Clone, Default)]
pub struct EquivocationEvidenceMailbox {
    queue:
        std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<VerifiedEquivocationEvidence>>>,
    notify: std::sync::Arc<tokio::sync::Notify>,
}

impl EquivocationEvidenceMailbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, evidence: VerifiedEquivocationEvidence) -> EquivocationPushOutcome {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if queue.iter().any(|pending| {
            (pending.accused_pubkey, pending.epoch) == (evidence.accused_pubkey, evidence.epoch)
                || pending.evidence_hash == evidence.evidence_hash
        }) {
            return EquivocationPushOutcome::Duplicate;
        }
        if queue.len() == 64 {
            return EquivocationPushOutcome::Full;
        }
        queue.push_back(evidence);
        self.notify.notify_one();
        EquivocationPushOutcome::Enqueued
    }
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

    fn evidence(signer: u8, epoch: u64, payload: u8) -> super::VerifiedEquivocationEvidence {
        let pubkey = [signer; 48];
        let first_block = super::pack_equivocation_evidence_block(&pubkey, &[1; 96], &[payload]);
        let second_block = super::pack_equivocation_evidence_block(&pubkey, &[2; 96], &[payload]);
        super::VerifiedEquivocationEvidence {
            kind: super::EquivocationEvidenceKind::ConflictingNotarize,
            epoch,
            accused_pubkey: pubkey,
            evidence_hash: canonical_equivocation_evidence_hash(&first_block, &second_block),
            first_block,
            second_block,
        }
    }

    #[test]
    fn mailbox_coalesces_signer_epoch_and_hash_without_replacing() {
        use super::{EquivocationEvidenceMailbox, EquivocationPushOutcome::*};
        let mailbox = EquivocationEvidenceMailbox::new();
        assert_eq!(mailbox.push(evidence(1, 2, 3)), Enqueued);
        assert_eq!(mailbox.clone().push(evidence(1, 2, 4)), Duplicate);
        let mut same_hash = evidence(2, 3, 4);
        same_hash.evidence_hash = evidence(1, 2, 3).evidence_hash;
        assert_eq!(mailbox.push(same_hash), Duplicate);
        assert_eq!(mailbox.push(evidence(1, 3, 5)), Enqueued);
        let queue = mailbox.queue.lock().unwrap();
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0].first_block, evidence(1, 2, 3).first_block);
    }

    #[test]
    fn mailbox_full_preserves_entries_and_checks_duplicates_first() {
        use super::{EquivocationEvidenceMailbox, EquivocationPushOutcome::*};
        let mailbox = EquivocationEvidenceMailbox::new();
        for signer in 0..64 {
            assert_eq!(mailbox.push(evidence(signer, 1, 2)), Enqueued);
        }
        assert_eq!(mailbox.push(evidence(64, 1, 2)), Full);
        assert_eq!(mailbox.push(evidence(0, 1, 3)), Duplicate);
        assert_eq!(mailbox.queue.lock().unwrap().len(), 64);
    }

    #[test]
    fn pack_preserves_exact_wire_bytes() {
        let block = super::pack_equivocation_evidence_block(&[1; 48], &[2; 96], &[3, 4]);
        assert_eq!(&block[..48], &[1; 48]);
        assert_eq!(&block[48..144], &[2; 96]);
        assert_eq!(&block[144..], &[3, 4]);
    }
}
