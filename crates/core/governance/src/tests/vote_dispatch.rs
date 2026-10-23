use alloy_primitives::keccak256;
use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent};
use outbe_primitives::addresses::{GOVERNANCE_ADDRESS, VOTE_ADDRESS};
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_vote::constants::{MAX_PAYLOAD_BYTES, VOTING_WINDOW_BLOCKS};
use outbe_vote::errors::VoteError;
use outbe_vote::handlers::{VoteTarget, VoteTargetRegistry};
use outbe_vote::precompile::IVote;
use outbe_vote::schema::Vote;
use outbe_vote::schema::{BondSettlement, ProposalStatus};

use crate::errors::GovernanceError;
use crate::precompile::IGovernance;
use crate::schema::GovernanceContract;
use crate::status;
use crate::vote_target::GovernanceVoteTarget;
use crate::vote_target::{GovernanceVotePayload, ProposalKind};

static GOVERNANCE_VOTE_TARGET: GovernanceVoteTarget = GovernanceVoteTarget;
static VOTE_HANDLERS: &[&dyn VoteTarget] = &[&GOVERNANCE_VOTE_TARGET];
static VOTE_TARGET_REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(VOTE_HANDLERS);

const PROPOSER: Address = address!("0x1111111111111111111111111111111111111111");
const VOTER_A: Address = address!("0x2222222222222222222222222222222222222222");
const VOTER_B: Address = address!("0x3333333333333333333333333333333333333333");

const KINDS: [ProposalKind; 2] = [ProposalKind::Oip, ProposalKind::Gip];

fn block_ctx(storage: StorageHandle, block_number: u64) -> BlockRuntimeContext {
    BlockRuntimeContext::new(BlockContext::empty_for_tests(block_number, 0, 1), storage)
}

fn with_vote_provider<F: FnOnce(StorageHandle)>(f: F) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(1);
    {
        let storage = StorageHandle::new(&mut provider);
        setup_validators(storage.clone());
        f(storage);
    }
    provider
}

fn with_vote<F: FnOnce(StorageHandle)>(f: F) {
    let _ = with_vote_provider(f);
}

fn setup_validators(storage: StorageHandle) {
    let owner = address!("0xffffffffffffffffffffffffffffffffffffffff");
    for (addr, seed) in [(PROPOSER, 1u8), (VOTER_A, 2), (VOTER_B, 3)] {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(owner).unwrap();
        vs.set_config_max_validators(100).unwrap();
        let mut pk = [0u8; 48];
        pk[0] = seed;
        vs.register_validator(owner, addr, &pk).unwrap();
        vs.activate_validator_via_boundary_for_test(addr).unwrap();
    }
}

fn process_begin_block_test(storage: StorageHandle, block_number: u64) {
    let ctx = block_ctx(storage.clone(), block_number);
    Vote::new(storage)
        .process_begin_block(&ctx, &VOTE_TARGET_REGISTRY)
        .unwrap();
}

fn create_and_pass(vote: &mut Vote<'_>, kind: ProposalKind, text: &str, current: u64) -> U256 {
    let proposal_id = vote
        .create_proposal(
            PROPOSER,
            GOVERNANCE_ADDRESS,
            &GovernanceVotePayload::encode(kind, text),
            current,
            &VOTE_TARGET_REGISTRY,
        )
        .unwrap();
    vote.cast_vote_approve(proposal_id, VOTER_A, true, current + 1)
        .unwrap();
    vote.cast_vote_approve(proposal_id, VOTER_B, true, current + 2)
        .unwrap();
    proposal_id
}

fn submitted_sig(kind: ProposalKind) -> B256 {
    match kind {
        ProposalKind::Oip => IGovernance::OipSubmitted::SIGNATURE_HASH,
        ProposalKind::Gip => IGovernance::GipSubmitted::SIGNATURE_HASH,
        ProposalKind::CanonText | ProposalKind::MetaCanonText => {
            unreachable!("text kinds create no proposal record")
        }
    }
}

fn assert_approved(gov: &GovernanceContract<'_>, kind: ProposalKind, text: &str) {
    let id = U256::from(1);
    let (record_status, author, body, listed) = match kind {
        ProposalKind::Oip => {
            let p = gov.get_oip(id).unwrap().unwrap();
            let listed = gov
                .oips_by_status(status::APPROVED, U256::ZERO, U256::from(10))
                .unwrap();
            (p.status, p.author, p.text, listed)
        }
        ProposalKind::Gip => {
            let p = gov.get_gip(id).unwrap().unwrap();
            let listed = gov
                .gips_by_status(status::APPROVED, U256::ZERO, U256::from(10))
                .unwrap();
            (p.status, p.author, p.text, listed)
        }
        ProposalKind::CanonText | ProposalKind::MetaCanonText => {
            unreachable!("text kinds create no proposal record")
        }
    };
    assert_eq!(record_status, status::APPROVED);
    assert_eq!(author, PROPOSER);
    assert_eq!(body, text);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
}

fn assert_absent(gov: &GovernanceContract<'_>, kind: ProposalKind) {
    let id = U256::from(1);
    match kind {
        ProposalKind::Oip => {
            assert!(gov.get_oip(id).unwrap().is_none());
            assert_eq!(gov.oip_count().unwrap(), 0);
        }
        ProposalKind::Gip => {
            assert!(gov.get_gip(id).unwrap().is_none());
            assert_eq!(gov.gip_count().unwrap(), 0);
        }
        ProposalKind::CanonText | ProposalKind::MetaCanonText => {
            unreachable!("text kinds create no proposal record")
        }
    }
}

#[test]
fn approved_vote_creates_approved_proposal() {
    for kind in KINDS {
        let text = match kind {
            ProposalKind::Oip => "oip body",
            ProposalKind::Gip => "gip body",
            ProposalKind::CanonText | ProposalKind::MetaCanonText => unreachable!(),
        };
        let provider = with_vote_provider(|storage| {
            let mut vote = Vote::new(storage.clone());
            let current = 100u64;
            let proposal_id = create_and_pass(&mut vote, kind, text, current);
            process_begin_block_test(storage.clone(), current + VOTING_WINDOW_BLOCKS + 1);

            assert_eq!(
                vote.proposals
                    .get(proposal_id)
                    .unwrap()
                    .unwrap()
                    .proposal_status()
                    .unwrap(),
                ProposalStatus::Approved
            );
            assert_approved(&GovernanceContract::new(storage), kind, text);
        });
        assert!(provider
            .get_events(GOVERNANCE_ADDRESS)
            .iter()
            .any(|log| log.topics().first() == Some(&submitted_sig(kind))));
    }
}

#[test]
fn invalid_json_payload_is_rejected_at_creation() {
    with_vote(|storage| {
        let mut vote = Vote::new(storage);
        let err = vote
            .create_proposal(
                PROPOSER,
                GOVERNANCE_ADDRESS,
                "not-json",
                200,
                &VOTE_TARGET_REGISTRY,
            )
            .unwrap_err();
        assert!(
            matches!(err, PrecompileError::RevertBytes(_))
                || err.to_string().contains("invalid proposal payload"),
            "unexpected error: {err:?}"
        );
    });
}

#[test]
fn unknown_kind_is_rejected_at_creation() {
    with_vote(|storage| {
        let mut vote = Vote::new(storage);
        let err = vote
            .create_proposal(
                PROPOSER,
                GOVERNANCE_ADDRESS,
                r#"{"kind":"xip","text":"body"}"#,
                200,
                &VOTE_TARGET_REGISTRY,
            )
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            PrecompileError::from(GovernanceError::InvalidPayload).to_string()
        );
    });
}

#[test]
fn empty_text_is_rejected_at_creation() {
    with_vote(|storage| {
        let mut vote = Vote::new(storage);
        let err = vote
            .create_proposal(
                PROPOSER,
                GOVERNANCE_ADDRESS,
                r#"{"kind":"oip","text":""}"#,
                200,
                &VOTE_TARGET_REGISTRY,
            )
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            PrecompileError::from(GovernanceError::EmptyText).to_string()
        );
    });
}

#[test]
fn oversize_raw_payload_is_rejected_at_creation() {
    let expected = PrecompileError::from(VoteError::InvalidPayload).to_string();
    for kind in KINDS {
        with_vote(|storage| {
            let mut vote = Vote::new(storage.clone());
            let err = vote
                .create_proposal(
                    PROPOSER,
                    GOVERNANCE_ADDRESS,
                    &GovernanceVotePayload::encode(kind, &"a".repeat(MAX_PAYLOAD_BYTES)),
                    200,
                    &VOTE_TARGET_REGISTRY,
                )
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                expected,
                "expected InvalidPayload, got {err:?}"
            );
            assert_absent(&GovernanceContract::new(storage), kind);
        });
    }
}

#[test]
fn approved_vote_creates_proposal_at_raw_payload_bound() {
    for kind in KINDS {
        let envelope_len = GovernanceVotePayload::encode(kind, "").len();
        let text = "a".repeat(MAX_PAYLOAD_BYTES - envelope_len);
        with_vote(|storage| {
            let mut vote = Vote::new(storage.clone());
            let current = 300u64;
            create_and_pass(&mut vote, kind, &text, current);
            process_begin_block_test(storage.clone(), current + VOTING_WINDOW_BLOCKS + 1);
            assert_approved(&GovernanceContract::new(storage), kind, &text);
        });
    }
}

#[test]
fn expired_governance_proposal_does_not_create_record() {
    for kind in KINDS {
        with_vote(|storage| {
            let mut vote = Vote::new(storage.clone());
            let current = 400u64;
            let proposal_id = vote
                .create_proposal(
                    PROPOSER,
                    GOVERNANCE_ADDRESS,
                    &GovernanceVotePayload::encode(kind, "should not land"),
                    current,
                    &VOTE_TARGET_REGISTRY,
                )
                .unwrap();
            vote.cast_vote_approve(proposal_id, VOTER_A, true, current + 1)
                .unwrap();

            process_begin_block_test(storage.clone(), current + VOTING_WINDOW_BLOCKS + 1);
            assert_eq!(
                vote.proposals
                    .get(proposal_id)
                    .unwrap()
                    .unwrap()
                    .proposal_status()
                    .unwrap(),
                ProposalStatus::Expired
            );
            assert_absent(&GovernanceContract::new(storage), kind);
        });
    }
}

// Seed only after admission and quorum, modelling capacity consumed while voting.
fn seed_capacity(storage: StorageHandle, kind: ProposalKind, author: u32, next_id: u64) {
    let gov = GovernanceContract::new(storage);
    match kind {
        ProposalKind::Oip => {
            gov.oip_author_count.write(&PROPOSER, author).unwrap();
            gov.next_oip_id.write(next_id).unwrap();
        }
        ProposalKind::Gip => {
            gov.gip_author_count.write(&PROPOSER, author).unwrap();
            gov.next_gip_id.write(next_id).unwrap();
        }
        ProposalKind::CanonText | ProposalKind::MetaCanonText => {
            unreachable!("text kinds have no record capacity")
        }
    }
}

fn admitted_fixture(kind: ProposalKind) -> (HashMapStorageProvider, U256) {
    let mut id = U256::ZERO;
    let mut provider = with_vote_provider(|storage| {
        id = create_and_pass(&mut Vote::new(storage), kind, "approved body", 100);
    });
    provider.clear_mutation_failure();
    (provider, id)
}

fn finalize(provider: &mut HashMapStorageProvider) -> outbe_primitives::error::Result<()> {
    let storage = StorageHandle::new(provider);
    Vote::new(storage.clone()).process_begin_block(
        &block_ctx(storage, 101 + VOTING_WINDOW_BLOCKS),
        &VOTE_TARGET_REGISTRY,
    )
}

fn assert_pending(storage: StorageHandle, id: U256) {
    let vote = Vote::new(storage);
    assert_eq!(
        vote.proposals
            .get(id)
            .unwrap()
            .unwrap()
            .proposal_status()
            .unwrap(),
        ProposalStatus::Pending
    );
    assert_eq!(vote.list_pending_proposal_ids().unwrap(), vec![id]);
    assert_eq!(
        vote.proposal_bond(id).unwrap().settlement,
        BondSettlement::NoBond
    );
}

#[test]
fn capacity_after_quorum_is_terminal_and_never_replayed() {
    for kind in KINDS {
        for (author, next_id) in [(u32::MAX, 0), (0, u64::MAX)] {
            let (mut provider, id) = admitted_fixture(kind);
            {
                let storage = StorageHandle::new(&mut provider);
                seed_capacity(storage.clone(), kind, author, next_id);
            }
            let before: std::collections::HashMap<_, _> = provider
                .storage
                .iter()
                .filter(|(key, _)| key.0 == GOVERNANCE_ADDRESS)
                .map(|(key, value)| (*key, *value))
                .collect();
            finalize(&mut provider).unwrap();
            {
                let storage = StorageHandle::new(&mut provider);
                let vote = Vote::new(storage.clone());
                assert_eq!(
                    vote.proposals
                        .get(id)
                        .unwrap()
                        .unwrap()
                        .proposal_status()
                        .unwrap(),
                    ProposalStatus::Error
                );
                assert!(vote.list_pending_proposal_ids().unwrap().is_empty());
                assert_eq!(
                    vote.proposal_bond(id).unwrap().settlement,
                    BondSettlement::NoBond
                );
                let gov = GovernanceContract::new(storage.clone());
                assert!(gov.get_oip(U256::ONE).unwrap().is_none());
                assert!(gov.get_gip(U256::ONE).unwrap().is_none());
            }
            let after: std::collections::HashMap<_, _> = provider
                .storage
                .iter()
                .filter(|(key, _)| key.0 == GOVERNANCE_ADDRESS)
                .map(|(key, value)| (*key, *value))
                .collect();
            assert_eq!(after, before);
            assert!(provider.get_events(GOVERNANCE_ADDRESS).is_empty());
            let logs = provider.get_events(VOTE_ADDRESS);
            assert_eq!(
                logs.iter()
                    .filter(
                        |log| log.topics().first() == Some(&IVote::ProposalErrored::SIGNATURE_HASH)
                    )
                    .count(),
                1
            );
            for signature in [
                IVote::ProposalApproved::SIGNATURE_HASH,
                IVote::ProposalBondRefunded::SIGNATURE_HASH,
                IVote::ProposalBondBurned::SIGNATURE_HASH,
            ] {
                assert!(!logs
                    .iter()
                    .any(|log| log.topics().first() == Some(&signature)));
            }
            let terminal = provider.storage.clone();
            let log_count = provider.get_events(VOTE_ADDRESS).len();
            finalize(&mut provider).unwrap();
            assert_eq!(provider.storage, terminal);
            assert_eq!(provider.get_events(VOTE_ADDRESS).len(), log_count);
            assert!(provider.get_events(GOVERNANCE_ADDRESS).is_empty());
        }
    }
}

#[test]
fn last_author_index_and_proposal_id_remain_usable() {
    for kind in KINDS {
        let (mut provider, id) = admitted_fixture(kind);
        seed_capacity(
            StorageHandle::new(&mut provider),
            kind,
            u32::MAX - 1,
            u64::MAX - 1,
        );
        finalize(&mut provider).unwrap();
        let storage = StorageHandle::new(&mut provider);
        let vote = Vote::new(storage.clone());
        assert_eq!(
            vote.proposals
                .get(id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Approved
        );
        assert!(vote.list_pending_proposal_ids().unwrap().is_empty());
        let gov = GovernanceContract::new(storage);
        let last = U256::from(u64::MAX);
        let (record_status, count, next, listed) = match kind {
            ProposalKind::Oip => (
                gov.get_oip(last).unwrap().unwrap().status,
                gov.oip_author_count.read(&PROPOSER).unwrap(),
                gov.next_oip_id.read().unwrap(),
                gov.oips_by_author(PROPOSER, U256::from(u32::MAX - 1), U256::ONE)
                    .unwrap(),
            ),
            ProposalKind::Gip => (
                gov.get_gip(last).unwrap().unwrap().status,
                gov.gip_author_count.read(&PROPOSER).unwrap(),
                gov.next_gip_id.read().unwrap(),
                gov.gips_by_author(PROPOSER, U256::from(u32::MAX - 1), U256::ONE)
                    .unwrap(),
            ),
            ProposalKind::CanonText | ProposalKind::MetaCanonText => {
                unreachable!("text kinds create no proposal record")
            }
        };
        assert_eq!(record_status, status::APPROVED);
        assert_eq!(count, u32::MAX);
        assert_eq!(next, u64::MAX);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, last);
    }
}

#[test]
fn public_submit_preserves_capacity_error_selectors() {
    for kind in KINDS {
        for (author, next_id, expected) in [
            (u32::MAX, 0, IGovernance::AuthorIndexFull {}.abi_encode()),
            (
                0,
                u64::MAX,
                IGovernance::ProposalIdExhausted {}.abi_encode(),
            ),
        ] {
            with_vote(|storage| {
                seed_capacity(storage.clone(), kind, author, next_id);
                let input = match kind {
                    ProposalKind::Oip => IGovernance::submitOipCall {
                        text: "body".into(),
                    }
                    .abi_encode(),
                    ProposalKind::Gip => IGovernance::submitGipCall {
                        text: "body".into(),
                    }
                    .abi_encode(),
                    ProposalKind::CanonText | ProposalKind::MetaCanonText => {
                        unreachable!("text kinds have no record capacity")
                    }
                };
                let error =
                    crate::precompile::dispatch(storage, &input, PROPOSER, U256::ZERO).unwrap_err();
                match error {
                    PrecompileError::RevertBytes(bytes) => {
                        assert_eq!(bytes.as_ref(), expected.as_slice())
                    }
                    other => panic!("expected ABI capacity error, got {other:?}"),
                }
            });
        }
    }
}

#[test]
fn corrupted_admitted_payloads_remain_pending_without_terminal_logs() {
    for kind in KINDS.into_iter().chain(TEXT_KINDS) {
        for payload in [
            "{".to_string(),
            r#"{"kind":"unknown","text":"body"}"#.to_string(),
            GovernanceVotePayload::encode(kind, ""),
            GovernanceVotePayload::encode(kind, &"x".repeat(crate::runtime::MAX_TEXT_BYTES + 1)),
        ] {
            let (mut provider, id) = admitted_fixture(kind);
            {
                let storage = StorageHandle::new(&mut provider);
                let vote = Vote::new(storage);
                let mut record = vote.proposals.get(id).unwrap().unwrap();
                record.payload = payload;
                vote.proposals.update(&record).unwrap();
            }
            let before = provider.storage.clone();
            let logs = provider.get_events(VOTE_ADDRESS).len();
            assert!(finalize(&mut provider).is_err());
            assert_eq!(provider.storage, before);
            assert_eq!(provider.get_events(VOTE_ADDRESS).len(), logs);
            assert!(provider.get_events(GOVERNANCE_ADDRESS).is_empty());
            assert_pending(StorageHandle::new(&mut provider), id);
        }
    }
}

// ------------------------------------------------- canon / meta-canon text ---

const TEXT_KINDS: [ProposalKind; 2] = [ProposalKind::CanonText, ProposalKind::MetaCanonText];

/// Reads `(text, version, hash, revision hash at version)` for a text kind.
fn stored_text(gov: &GovernanceContract<'_>, kind: ProposalKind) -> (String, u64, B256, B256) {
    let ((text, version, hash), revision) = match kind {
        ProposalKind::CanonText => {
            let stored = gov.get_canon().unwrap();
            let revision = gov.canon_revision_hash(stored.1).unwrap();
            (stored, revision)
        }
        ProposalKind::MetaCanonText => {
            let stored = gov.get_meta_canon().unwrap();
            let revision = gov.meta_canon_revision_hash(stored.1).unwrap();
            (stored, revision)
        }
        ProposalKind::Oip | ProposalKind::Gip => unreachable!("record kinds have no text slot"),
    };
    (text, version, hash, revision)
}

/// Decodes every `(version, hash)` text-update event of `kind`, in emit order.
fn text_events(provider: &HashMapStorageProvider, kind: ProposalKind) -> Vec<(u64, B256)> {
    provider
        .get_events(GOVERNANCE_ADDRESS)
        .iter()
        .filter_map(|log| match kind {
            ProposalKind::CanonText => IGovernance::CanonUpdated::decode_log_data(log)
                .ok()
                .map(|e| (e.version, e.hash)),
            ProposalKind::MetaCanonText => IGovernance::MetaCanonUpdated::decode_log_data(log)
                .ok()
                .map(|e| (e.version, e.hash)),
            ProposalKind::Oip | ProposalKind::Gip => {
                unreachable!("record kinds emit no text event")
            }
        })
        .collect()
}

/// Installs version 1 through the bootstrap route, then replaces it with
/// version 2 through an approved vote proposal of `kind`.
fn assert_approved_text_proposal_changes_text(kind: ProposalKind) {
    let provider = with_vote_provider(|storage| {
        let mut gov = GovernanceContract::new(storage.clone());
        match kind {
            ProposalKind::CanonText => gov.update_canon(PROPOSER, "text v1").unwrap(),
            ProposalKind::MetaCanonText => gov.update_meta_canon(PROPOSER, "text v1").unwrap(),
            ProposalKind::Oip | ProposalKind::Gip => unreachable!("record kinds have no text slot"),
        };

        let mut vote = Vote::new(storage.clone());
        let current = 100u64;
        let proposal_id = create_and_pass(&mut vote, kind, "text v2", current);
        // Before the tally the text is still version 1.
        assert_eq!(stored_text(&gov, kind).1, 1);
        process_begin_block_test(storage.clone(), current + VOTING_WINDOW_BLOCKS + 1);

        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Approved
        );
        let (text, version, hash, revision) = stored_text(&gov, kind);
        assert_eq!(text, "text v2");
        assert_eq!(version, 2);
        assert_eq!(hash, keccak256(b"text v2"));
        assert_eq!(revision, hash);
        // The text kinds create no OIP / GIP record.
        assert_eq!(gov.oip_count().unwrap(), 0);
        assert_eq!(gov.gip_count().unwrap(), 0);
    });
    assert_eq!(
        text_events(&provider, kind),
        vec![(1, keccak256(b"text v1")), (2, keccak256(b"text v2"))]
    );
}

#[test]
fn approved_canon_proposal_changes_the_text() {
    assert_approved_text_proposal_changes_text(ProposalKind::CanonText);
}

#[test]
fn approved_meta_canon_proposal_changes_the_meta_text() {
    assert_approved_text_proposal_changes_text(ProposalKind::MetaCanonText);
}

#[test]
fn approved_canon_proposal_installs_first_text() {
    let provider = with_vote_provider(|storage| {
        let mut vote = Vote::new(storage.clone());
        create_and_pass(&mut vote, ProposalKind::CanonText, "first", 100);
        process_begin_block_test(storage.clone(), 100 + VOTING_WINDOW_BLOCKS + 1);
        let (text, version, _, _) =
            stored_text(&GovernanceContract::new(storage), ProposalKind::CanonText);
        assert_eq!((text.as_str(), version), ("first", 1));
    });
    assert_eq!(
        text_events(&provider, ProposalKind::CanonText),
        vec![(1, keccak256(b"first"))]
    );
}

#[test]
fn unshaped_text_rejected_at_proposal_creation() {
    let too_big = "a".repeat(crate::runtime::MAX_TEXT_BYTES + 1);
    for kind in TEXT_KINDS {
        for (text, expected) in [
            (
                "",
                PrecompileError::from(crate::errors::GovernanceError::EmptyText),
            ),
            (
                too_big.as_str(),
                PrecompileError::from(outbe_vote::errors::VoteError::InvalidPayload),
            ),
        ] {
            with_vote(|storage| {
                let mut vote = Vote::new(storage.clone());
                let err = vote
                    .create_proposal(
                        PROPOSER,
                        GOVERNANCE_ADDRESS,
                        &GovernanceVotePayload::encode(kind, text),
                        200,
                        &VOTE_TARGET_REGISTRY,
                    )
                    .unwrap_err();
                assert_eq!(
                    err.to_string(),
                    expected.to_string(),
                    "kind={kind:?} err={err:?}"
                );
                assert_eq!(stored_text(&GovernanceContract::new(storage), kind).1, 0);
            });
        }
    }
}

#[test]
fn every_governance_and_vote_finalization_mutation_rolls_back_and_retries() {
    for kind in KINDS.into_iter().chain(TEXT_KINDS) {
        // Sweep both successful target writes and terminal-capacity finalization.
        for exhausted in [false, true] {
            if exhausted && matches!(kind, ProposalKind::CanonText | ProposalKind::MetaCanonText) {
                continue;
            }
            let fixture = || {
                let (mut provider, id) = admitted_fixture(kind);
                if exhausted {
                    seed_capacity(StorageHandle::new(&mut provider), kind, u32::MAX, 0);
                }
                provider.clear_mutation_failure();
                (provider, id)
            };
            let (mut baseline, _) = fixture();
            finalize(&mut baseline).unwrap();
            let mutations = baseline.clear_mutation_failure();
            assert!(mutations > 0);
            for operation in 0..mutations {
                let (mut provider, id) = fixture();
                let before = provider.storage.clone();
                let logs = provider.get_events(VOTE_ADDRESS).len();
                provider.fail_after_mutation_at(operation);
                let error = finalize(&mut provider).unwrap_err();
                assert!(
                    matches!(error, PrecompileError::Storage(_)),
                    "{kind:?}, exhausted={exhausted}, operation={operation}: {error:?}"
                );
                provider.clear_mutation_failure();
                assert_eq!(provider.storage, before, "operation {operation}");
                assert_eq!(provider.get_events(VOTE_ADDRESS).len(), logs);
                assert!(provider.get_events(GOVERNANCE_ADDRESS).is_empty());
                assert_pending(StorageHandle::new(&mut provider), id);
                finalize(&mut provider).unwrap();
                assert_eq!(
                    provider.storage, baseline.storage,
                    "retry operation {operation}"
                );
                assert_eq!(
                    provider.get_events(VOTE_ADDRESS),
                    baseline.get_events(VOTE_ADDRESS)
                );
                assert_eq!(
                    provider.get_events(GOVERNANCE_ADDRESS),
                    baseline.get_events(GOVERNANCE_ADDRESS)
                );
            }
        }
    }
}
