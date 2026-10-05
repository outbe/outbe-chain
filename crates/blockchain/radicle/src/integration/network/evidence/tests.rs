use super::*;
use crate::{
    endpoint::{sign_response, EndpointAddress, EndpointFrame, EndpointResponseBody},
    manager::{FinalizedBlock, FinalizedValidator},
};
use alloy_primitives::{Address, B256};
use commonware_cryptography::{bls12381, Signer as _};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn proof(seed: u64, valid_until: u64) -> TestResult<SignedEndpointEvidence> {
    let signer = bls12381::PrivateKey::from_seed(seed);
    let response = sign_response(
        EndpointResponseBody {
            request_id: [1; 32],
            chain_id: 1,
            genesis_hash: B256::ZERO,
            validator: Address::repeat_byte(seed as u8),
            node_id: [seed as u8; 32],
            addresses: vec![EndpointAddress::dns("evidence.example", 8776)?],
            anchor_number: 1,
            anchor_hash: B256::with_last_byte(1),
            valid_until,
        },
        &signer,
    )?;
    Ok(SignedEndpointEvidence {
        peer: PeerId::from_public_key(&signer.public_key()),
        encoded_frame: EndpointFrame::Response(Box::new(response.clone())).encode(),
        response,
    })
}

fn finalized(proof: &SignedEndpointEvidence) -> FinalizedSnapshot {
    FinalizedSnapshot {
        block: FinalizedBlock {
            number: 10,
            hash: B256::with_last_byte(10),
        },
        validators: vec![FinalizedValidator {
            address: proof.response.body().validator,
            peer: proof.peer,
            node_id: Some(proof.response.body().node_id),
        }],
        registry_generation: 0,
        repositories: vec![],
    }
}

#[test]
fn snapshot_remains_available_after_writer_unwinds() -> TestResult {
    let evidence = EndpointEvidenceHandle::default();
    let original = proof(1, 11)?;
    evidence.publish(original.clone());
    let fault = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = evidence.0.write();
        std::panic::resume_unwind(Box::new("writer unwind witness"));
    }));
    assert!(fault.is_err());
    assert_eq!(evidence.snapshot(), vec![original]);
    let replacement = proof(1, 12)?;
    evidence.publish(replacement.clone());
    assert_eq!(evidence.snapshot(), vec![replacement.clone()]);
    let mut expired = finalized(&replacement);
    expired.block.number = 12;
    evidence.prune(&expired);
    assert!(evidence.snapshot().is_empty());
    Ok(())
}

#[test]
fn publication_remains_available_after_reader_unwinds() -> TestResult {
    let evidence = EndpointEvidenceHandle::default();
    let fault = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = evidence.0.read();
        std::panic::resume_unwind(Box::new("reader unwind witness"));
    }));
    assert!(fault.is_err());
    let published = proof(1, 11)?;
    evidence.publish(published.clone());
    assert_eq!(evidence.snapshot(), vec![published]);
    Ok(())
}

#[test]
fn snapshots_are_ordered_independent_copies() -> TestResult {
    let evidence = EndpointEvidenceHandle::default();
    let mut expected = vec![proof(1, 11)?, proof(2, 11)?, proof(3, 11)?];
    expected.sort_by_key(|proof| proof.peer);
    for proof in expected.iter().rev() {
        evidence.publish(proof.clone());
    }
    let mut copied = evidence.snapshot();
    assert_eq!(copied, expected);
    copied[0].encoded_frame.clear();
    copied.reverse();
    assert_eq!(evidence.snapshot(), expected);
    Ok(())
}

#[test]
fn replacing_peer_preserves_previously_returned_snapshot() -> TestResult {
    let evidence = EndpointEvidenceHandle::default();
    let original = proof(1, 11)?;
    evidence.publish(original.clone());
    let before = evidence.snapshot();
    let replacement = proof(1, 12)?;
    evidence.publish(replacement.clone());
    assert_eq!(before, vec![original]);
    assert_eq!(evidence.snapshot(), vec![replacement]);
    Ok(())
}

#[test]
fn pruning_requires_live_exact_validator_peer_and_node_binding() -> TestResult {
    let published = proof(1, 11)?;
    let current = finalized(&published);
    let mut expired = current.clone();
    expired.block.number = 11;
    let mut removed = current.clone();
    removed.validators.clear();
    let mut wrong_validator = current.clone();
    wrong_validator.validators[0].address = Address::repeat_byte(9);
    let mut wrong_peer = current.clone();
    wrong_peer.validators[0].peer = proof(2, 11)?.peer;
    let mut wrong_node = current.clone();
    wrong_node.validators[0].node_id = Some([9; 32]);
    let mut missing_node = current.clone();
    missing_node.validators[0].node_id = None;
    let mut split_binding = wrong_validator.clone();
    split_binding
        .validators
        .push(wrong_peer.validators[0].clone());
    for snapshot in [
        expired,
        removed,
        wrong_validator,
        wrong_peer,
        wrong_node,
        missing_node,
        split_binding,
    ] {
        let evidence = EndpointEvidenceHandle::default();
        evidence.publish(published.clone());
        evidence.prune(&snapshot);
        assert!(evidence.snapshot().is_empty(), "snapshot: {snapshot:?}");
    }
    let evidence = EndpointEvidenceHandle::default();
    evidence.publish(published.clone());
    evidence.prune(&current);
    assert_eq!(evidence.snapshot(), vec![published]);
    Ok(())
}

#[test]
fn concurrent_readers_observe_complete_signed_records() -> TestResult {
    let evidence = EndpointEvidenceHandle::default();
    evidence.publish(proof(1, 11)?);
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let barrier = &barrier;
        let reader = &evidence;
        let reading = scope.spawn(move || -> TestResult {
            barrier.wait();
            for _ in 0..100 {
                let snapshot = reader.snapshot();
                assert_eq!(snapshot.len(), 1);
                let record = &snapshot[0];
                assert_eq!(
                    EndpointFrame::decode(&record.encoded_frame)?,
                    EndpointFrame::Response(Box::new(record.response.clone()))
                );
            }
            Ok(())
        });
        barrier.wait();
        for valid_until in 11..20 {
            evidence.publish(proof(1, valid_until)?);
        }
        match reading.join() {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}
