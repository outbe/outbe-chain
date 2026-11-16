use std::{collections::BTreeMap, time::Duration};

use alloy_primitives::{keccak256, Address, B256};
use outbe_primitives::equivocation_evidence::VerifiedEquivocationEvidence;
use tokio::time::Instant;

pub(super) type PendingSlashMap = BTreeMap<(Address, u64), PendingSlash>;

pub(super) struct PendingSlash {
    pub evidence: VerifiedEquivocationEvidence,
    pub next_attempt_at: Instant,
    pub ranked: bool,
    pub submitted_tx: Option<(B256, u64, u128)>,
}

pub(super) fn coalesce(
    pending: &mut PendingSlashMap,
    accused: Address,
    evidence: VerifiedEquivocationEvidence,
) {
    pending
        .entry((accused, evidence.epoch))
        .or_insert_with(|| PendingSlash {
            evidence,
            next_attempt_at: Instant::now(),
            ranked: false,
            submitted_tx: None,
        });
}

pub(super) fn rank_delay(
    epoch: u64,
    accused: Address,
    local: Address,
    active_committee: impl IntoIterator<Item = Address>,
    min_block_time: Duration,
) -> Duration {
    let mut candidates: Vec<_> = active_committee
        .into_iter()
        .filter(|candidate| *candidate != accused)
        .map(|candidate| {
            let mut bytes = [0; 48];
            bytes[..8].copy_from_slice(&epoch.to_be_bytes());
            bytes[8..28].copy_from_slice(accused.as_slice());
            bytes[28..].copy_from_slice(candidate.as_slice());
            (keccak256(bytes), candidate)
        })
        .collect();
    candidates.sort_unstable();
    let rank = candidates
        .iter()
        .position(|(_, candidate)| *candidate == local)
        .unwrap_or(candidates.len());
    delay(rank, min_block_time)
}

fn delay(rank: usize, min_block_time: Duration) -> Duration {
    let blocks = match rank {
        0 => 0,
        1..=30 => 2 * rank,
        _ => 60 + (rank % 16) * 2,
    };
    // blocks <= 90
    min_block_time.saturating_mul(u32::try_from(blocks).unwrap_or(u32::MAX))
}

/// A replacement of our committed-nonce transaction takes precedence over queued siblings.
pub(super) fn fee_for_attempt(
    submitted: Option<(B256, u64, u128)>,
    nonce: u64,
    base_fee: u64,
    pool: &[(B256, u64)],
) -> Result<Option<u128>, &'static str> {
    if let Some((hash, own_nonce, old_fee)) = submitted {
        if own_nonce == nonce && pool.contains(&(hash, nonce)) {
            return Ok((old_fee < u128::from(base_fee)).then(|| {
                u128::from(base_fee)
                    .saturating_mul(2)
                    .max(old_fee.saturating_mul(125).div_ceil(100))
            }));
        }
    }
    if !pool.is_empty() {
        return Err("sender_busy");
    }
    Ok(Some((u128::from(base_fee).saturating_mul(3) / 2).max(
        u128::from(alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::equivocation_evidence::EquivocationEvidenceKind;

    fn evidence(epoch: u64, hash: u8) -> VerifiedEquivocationEvidence {
        VerifiedEquivocationEvidence {
            kind: EquivocationEvidenceKind::ConflictingNotarize,
            epoch,
            accused_pubkey: [1; 48],
            first_block: vec![hash],
            second_block: vec![2],
            evidence_hash: B256::repeat_byte(hash),
        }
    }

    #[test]
    fn coalescing_keeps_first_pair_and_submission_state() {
        let mut map = PendingSlashMap::new();
        for i in 0..64 {
            coalesce(&mut map, Address::repeat_byte(i), evidence(1, i));
        }
        let key = (Address::ZERO, 1);
        map.get_mut(&key).unwrap().submitted_tx = Some((B256::ZERO, 7, 100));
        let deadline = map[&key].next_attempt_at;
        coalesce(&mut map, Address::ZERO, evidence(1, 99));
        assert_eq!(map.len(), 64);
        assert_eq!(map[&key].evidence.evidence_hash, B256::ZERO);
        assert_eq!(map[&key].submitted_tx, Some((B256::ZERO, 7, 100)));
        assert_eq!(map[&key].next_attempt_at, deadline);
        coalesce(&mut map, Address::ZERO, evidence(2, 99));
        assert!(map.contains_key(&(Address::ZERO, 2)));
    }

    #[test]
    fn full_pending_map_drains_known_pairs_without_losing_blocked_oldest() {
        use outbe_primitives::equivocation_evidence::{
            EquivocationEvidenceMailbox, EquivocationPushOutcome,
        };
        let mut map = PendingSlashMap::new();
        for i in 0..64 {
            coalesce(&mut map, Address::repeat_byte(i), evidence(1, i));
        }
        let mailbox = EquivocationEvidenceMailbox::new();
        let mut waiting = evidence(1, 70);
        waiting.accused_pubkey = [70; 48];
        assert_eq!(mailbox.push(waiting), EquivocationPushOutcome::Enqueued);
        assert_eq!(
            mailbox.push(evidence(1, 99)),
            EquivocationPushOutcome::Enqueued
        );
        super::super::drain_resolved(&mailbox, &mut map, |ev| {
            Ok(Address::repeat_byte(ev.accused_pubkey[0]))
        })
        .unwrap();
        assert_eq!(map.len(), 64);
        assert_eq!(
            map[&(Address::repeat_byte(1), 1)].evidence.evidence_hash,
            B256::repeat_byte(1)
        );
        map.remove(&(Address::ZERO, 1));
        super::super::drain_resolved(&mailbox, &mut map, |ev| {
            assert_eq!(ev.evidence_hash, B256::repeat_byte(70));
            Ok(Address::repeat_byte(70))
        })
        .unwrap();
        assert_eq!(map.len(), 64);
        assert!(mailbox.pop_if(|_| true).is_none());
    }

    #[test]
    fn unregistered_signers_never_occupy_pending_slots() {
        use outbe_primitives::equivocation_evidence::EquivocationEvidenceMailbox;
        for initial in [0, 64] {
            let mailbox = EquivocationEvidenceMailbox::new();
            for signer in [200, 201] {
                let mut ev = evidence(1, signer);
                ev.accused_pubkey = [signer; 48];
                mailbox.push(ev);
            }
            let mut map = PendingSlashMap::new();
            for signer in 1..=initial {
                coalesce(&mut map, Address::repeat_byte(signer), evidence(1, signer));
            }
            let mut resolved = 0;
            super::super::drain_resolved(&mailbox, &mut map, |_| {
                resolved += 1;
                Ok(Address::ZERO)
            })
            .unwrap();
            assert_eq!(resolved, 2);
            assert_eq!(map.len(), usize::from(initial));
            assert!(!map.contains_key(&(Address::ZERO, 1)));
            assert!(mailbox.is_empty());
        }
    }

    #[test]
    fn empty_mailbox_never_resolves_a_signer() {
        use outbe_primitives::equivocation_evidence::EquivocationEvidenceMailbox;
        let mailbox = EquivocationEvidenceMailbox::new();
        let mut map = PendingSlashMap::new();
        super::super::drain_resolved(&mailbox, &mut map, |_| {
            panic!("empty mailbox must not resolve a signer")
        })
        .unwrap();
        assert!(mailbox.is_empty());
    }

    #[tokio::test]
    async fn resolution_error_preserves_evidence_without_renotifying_or_consuming_capacity() {
        use outbe_primitives::{
            equivocation_evidence::EquivocationEvidenceMailbox, error::PrecompileError,
        };
        let mailbox = EquivocationEvidenceMailbox::new();
        mailbox.push(evidence(1, 3));
        mailbox.notified().await;
        let mut map = PendingSlashMap::new();
        let error = super::super::drain_resolved(&mailbox, &mut map, |_| {
            Err(PrecompileError::Storage("transient".into()).into())
        })
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<PrecompileError>(),
            Some(PrecompileError::Storage(_))
        ));
        assert!(map.is_empty());
        assert!(
            tokio::time::timeout(Duration::from_millis(1), mailbox.notified())
                .await
                .is_err()
        );
        assert_eq!(
            mailbox.pop_if(|_| true).unwrap().evidence_hash,
            B256::repeat_byte(3)
        );
    }

    #[test]
    fn drain_errors_back_off_without_blocking_other_task_deadlines() {
        use super::super::drain_when_due;
        use outbe_primitives::error::PrecompileError;
        let block = Duration::from_secs(60);
        let before = Instant::now();
        let mut retry_at = None;
        drain_when_due(&mut retry_at, block, || {
            Err(PrecompileError::Storage("transient".into()).into())
        });
        assert!(retry_at.unwrap() >= before + block);
        drain_when_due(&mut retry_at, block, || {
            panic!("must honor admission backoff")
        });
        let task_deadline = before + Duration::from_secs(1);
        assert_eq!(
            [task_deadline].into_iter().chain(retry_at).min(),
            Some(task_deadline)
        );
        retry_at = Some(before);
        drain_when_due(&mut retry_at, block, || Ok(()));
        assert!(retry_at.is_none());
    }

    #[tokio::test]
    async fn no_deadline_stays_idle_until_an_external_event() {
        assert!(
            tokio::time::timeout(Duration::from_millis(1), super::super::wait_until(None))
                .await
                .is_err()
        );
        tokio::time::timeout(
            Duration::from_secs(1),
            super::super::wait_until(Some(Instant::now())),
        )
        .await
        .expect("due deadline wakes");
    }

    #[test]
    fn delay_buckets_and_accused_exclusion_match_contract() {
        let block = Duration::from_millis(3);
        for (rank, blocks) in [(0, 0), (1, 2), (30, 60), (31, 90), (32, 60), (255, 90)] {
            assert_eq!(delay(rank, block), block * blocks);
        }
        let accused = Address::repeat_byte(1);
        let local = Address::repeat_byte(2);
        assert_eq!(
            rank_delay(7, accused, local, [accused, local], block),
            Duration::ZERO
        );
        assert_eq!(
            rank_delay(7, accused, Address::ZERO, [accused, local], block),
            block * 2
        );
        let candidates = [Address::repeat_byte(3), local, Address::repeat_byte(4)];
        let mut sorted = candidates.to_vec();
        sorted.sort_by_key(|candidate| {
            let mut bytes = 7u64.to_be_bytes().to_vec();
            bytes.extend_from_slice(accused.as_slice());
            bytes.extend_from_slice(candidate.as_slice());
            (keccak256(bytes), *candidate)
        });
        for (rank, candidate) in sorted.into_iter().enumerate() {
            assert_eq!(
                rank_delay(7, accused, candidate, candidates, block),
                delay(rank, block)
            );
        }
    }

    #[test]
    fn own_basefee_replacement_precedes_queued_siblings_and_keeps_nonce() {
        let hash = B256::repeat_byte(1);
        let queued = B256::repeat_byte(2);
        assert_eq!(
            fee_for_attempt(Some((hash, 7, 100)), 7, 101, &[(hash, 7), (queued, 8)]),
            Ok(Some(202))
        );
        assert_eq!(
            fee_for_attempt(Some((hash, 7, 100)), 7, 100, &[(hash, 7), (queued, 8)]),
            Ok(None)
        );
        assert_eq!(
            fee_for_attempt(Some((hash, 6, 100)), 7, 101, &[(queued, 8)]),
            Err("sender_busy")
        );
        assert_eq!(
            fee_for_attempt(None, 7, 101, &[(queued, 7)]),
            Err("sender_busy")
        );
        assert_eq!(fee_for_attempt(None, 7, 100, &[]), Ok(Some(150)));
        assert_eq!(
            fee_for_attempt(None, 7, 0, &[]),
            Ok(Some(u128::from(alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE)))
        );
    }
}
