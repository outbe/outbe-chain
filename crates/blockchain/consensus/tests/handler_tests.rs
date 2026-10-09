//! - handler / selector behaviour tests
//!
//! The full ApplicationHandler is wired across the whole consensus stack.
//! `stack_tests.rs` exercises it end-to-end. The tests here pin the V1
//! selector removal at the API surface. They also exercise the non-blocking
//! `ParentProofSelector` against the same proof-store substrate that the proposer
//! reads in production.
//!
//! - `missing_direct_parent_proof_does_not_wait_for_future_finalization` -
//!   (the V1 polling waiter is gone, and the selector returns synchronously).

use alloy_primitives::B256;
use outbe_consensus::finalization::{
    parent_cert_store::{
        CertifiedParentProofRecord, CertifiedParentProofStore, FinalizedParentCertStore, ProofKind,
    },
    selection::ParentProofSelector,
};
use outbe_primitives::consensus_metadata::ParentParticipationProof;
use std::time::Instant;

fn record(
    block_number: u64,
    hash: B256,
    proof_type: ParentParticipationProof,
) -> CertifiedParentProofRecord {
    let kind = match proof_type {
        ParentParticipationProof::Finalization => ProofKind::Finalization {
            finalized_block_number: block_number,
        },
        ParentParticipationProof::CertifiedNotarization => ProofKind::CertifiedNotarization,
    };
    CertifiedParentProofRecord {
        kind,
        finalized_block_hash: hash,
        ..CertifiedParentProofRecord::default()
    }
}

fn walk_source_files(dir: &std::path::Path, visit: &mut impl FnMut(&std::path::Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_source_files(&path, visit);
        } else if is_non_test_rust_source(&path) {
            visit(&path);
        }
    }
}

fn is_non_test_rust_source(path: &std::path::Path) -> bool {
    if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
        return false;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    !name.contains("_tests") && !name.starts_with("test_")
}

fn removed_api_hits(path: &std::path::Path) -> usize {
    let Ok(content) = std::fs::read_to_string(path) else {
        return 0;
    };
    content
        .lines()
        .filter(|line| {
            if !line.contains("await_parent_cert") || line.trim_start().starts_with("//") {
                return false;
            }
            eprintln!("await_parent_cert hit in {}: {}", path.display(), line);
            true
        })
        .count()
}

/// `rg -n "await_parent_cert" crates/blockchain/consensus/src/`
/// must return 0 hits in non-test code. This test does the check in-process, so
/// the assertion runs on every `cargo nextest` invocation.
#[test]
fn await_parent_cert_is_removed_from_non_test_consensus_src() {
    let mut total_non_test_hits = 0usize;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    walk_source_files(&root, &mut |path| {
        total_non_test_hits += removed_api_hits(path)
    });
    assert_eq!(
        total_non_test_hits, 0,
        "await_parent_cert must not exist in non-test consensus src code"
    );
}

#[test]
fn certified_notarized_parent_does_not_block_proposal() {
    // With a CertifiedNotarization record for the requested parent,
    // the selector returns synchronously. It does no polling and no
    // future-finalization wait. The V1 path could deadlock here (view 61 reproduction).
    let store = FinalizedParentCertStore::new();
    let hash = B256::with_last_byte(0xAA);
    store
        .put_certified_notarization(record(
            42,
            hash,
            ParentParticipationProof::CertifiedNotarization,
        ))
        .unwrap();
    let selector =
        ParentProofSelector::new(store, outbe_consensus::config::DEFAULT_PROPOSAL_TIMEOUT);

    let start = Instant::now();
    // The non-wait selector treats a certified-notarization record as
    // witness-only. It returns `None` synchronously (no polling, no
    // future-finalization wait), so a CN parent cannot deadlock the proposer.
    let result = selector.select_direct_parent_proof(0, 0, 42, hash);
    let elapsed = start.elapsed();

    assert!(
        result.is_none(),
        "certified-notarization is witness-only on the non-wait path"
    );
    // The CN witness remains in the store and, once promoted by the selector,
    // projects to V2 metadata at the known parent block number.
    let key =
        outbe_consensus::finalization::parent_cert_store::CertifiedParentProofKey::new(0, 0, hash);
    let witness = selector
        .parent_cert_store()
        .get_certified_notarization(key)
        .expect("CN witness must remain in the store");
    assert_eq!(
        witness.proof_kind(),
        ParentParticipationProof::CertifiedNotarization
    );
    let metadata = witness.to_v2_metadata(42);
    assert_eq!(metadata.finalized_block_number, 42);
    assert_eq!(metadata.finalized_block_hash, hash);
    // budget: synchronous lookup completes in microseconds, not the
    // legacy 25 ms poll interval.
    assert!(
        elapsed.as_millis() < 10,
        "selector must be non-blocking; took {elapsed:?}"
    );
}

#[test]
fn finalized_parent_uses_finalization_proof_when_available() {
    // across two slots: both finalization AND certified-notarization
    // present for the same parent -> finalization wins.
    let store = FinalizedParentCertStore::new();
    let hash = B256::with_last_byte(0xBB);
    store
        .put_certified_notarization(record(
            7,
            hash,
            ParentParticipationProof::CertifiedNotarization,
        ))
        .unwrap();
    store
        .put_finalization(record(7, hash, ParentParticipationProof::Finalization))
        .unwrap();
    let selector =
        ParentProofSelector::new(store, outbe_consensus::config::DEFAULT_PROPOSAL_TIMEOUT);

    let result = selector.select_direct_parent_proof(0, 0, 7, hash).unwrap();
    assert_eq!(result.proof_kind(), ParentParticipationProof::Finalization);
}

#[test]
fn missing_direct_parent_proof_does_not_wait_for_future_finalization() {
    // empty store -> selector returns None immediately. The V1
    // `await_parent_cert` polled until timeout (terminal-view halt root
    // cause). The new selector is synchronous.
    let store = FinalizedParentCertStore::new();
    let selector =
        ParentProofSelector::new(store, outbe_consensus::config::DEFAULT_PROPOSAL_TIMEOUT);

    let start = Instant::now();
    let result = selector.select_direct_parent_proof(0, 0, 42, B256::with_last_byte(0xCC));
    let elapsed = start.elapsed();

    assert!(result.is_none());
    assert!(
        elapsed.as_millis() < 10,
        "selector must not poll; took {elapsed:?}"
    );
}
