//! Actual automatic workers, native sidecars, protected SDK sockets and registry admission.
//! DKG dealing, canonical finalization and Commonware transport are fixtures, not production E2E.
#[path = "governments/support.rs"]
mod support;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall as _;
use commonware_codec::Encode as _;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_utils::TryCollect as _;
use outbe_coderegistry::{
    payload::PatchVotePayloadV1, vote_target::CodePatchVoteTarget, CodeRegistry,
};
use outbe_consensus::{bls::bootstrap_dkg_for_participants, hybrid::VrfMaterialProvider};
use outbe_primitives::{
    addresses::CODE_REGISTRY_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    radicle_publication::{
        PublicationBody, RepositoryPublicationMailbox, MAX_PUBLICATION_BATCH_LEN,
    },
    storage::{hashmap::HashMapStorageProvider, PrecompileStorageProvider as _},
};
use outbe_radicle::{
    endpoint::{ChainIdentity, EndpointAddress, HANDLE_DEADLINE},
    integration::{
        local::{LocalReceiptClient, LocalReceiptServer},
        publication::store::ReviewPackageStore,
        EndpointNetwork, EndpointSigningIdentity, EndpointTransport, LocalEndpointIdentity,
        LocalEndpointIdentityChannel, RadicleStatusChannel,
    },
    manager::{RepositoryReceiptReader as _, RethSnapshotReader},
};
use outbe_radicle_runtime::{Options, PreparedSidecar};
use outbe_validatorset::{
    contract::ValidatorSet,
    state::{write_committee_snapshot, CommitteeEntry, CommitteeSnapshot},
};
use outbe_vote::{
    handlers::{VoteTarget, VoteTargetRegistry},
    precompile::{dispatch_with_handlers, IVote},
    Vote,
};
use radicle::{
    cob::{
        self,
        patch::{self, Patches},
        store::access::WriteAs,
    },
    crypto::{ssh::Keystore, Seed, Signer as _, SigningKey},
    git,
    identity::RepoId,
    node::{Handle as _, Node, NodeId},
    profile::Home,
    storage::{SignRepository as _, WriteRepository as _},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::{Read as _, Write as _},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};
use support::{FinalizedProvider, RoutedReceiver, RoutedSender};
use tokio::sync::{mpsc as async_mpsc, oneshot};

struct TestLogger;
impl log::Log for TestLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == "outbe-radicle" || metadata.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }
    fn flush(&self) {}
}

struct Sidecar {
    home: Home,
    control: PathBuf,
    node: Node,
    node_id: NodeId,
    status: SocketAddr,
    signal: Option<mpsc::SyncSender<radicle_signals::Signal>>,
    running: Option<thread::JoinHandle<Result<(), outbe_radicle_runtime::Error>>>,
    _directory: tempfile::TempDir,
}
impl Sidecar {
    fn new(seed: u8) -> Self {
        let directory = tempfile::tempdir_in(if cfg!(target_os = "macos") {
            "/private/tmp"
        } else {
            "/tmp"
        })
        .unwrap();
        let home = Home::new(directory.path()).unwrap();
        let node_id = Keystore::new(&home.keys())
            .init("transport-only", None, Seed::new([seed; 32]))
            .unwrap();
        let control = home.node().join("control.sock");
        let mut result = Self {
            node: Node::new(&control),
            control,
            home,
            node_id,
            status: "127.0.0.1:0".parse().unwrap(),
            signal: None,
            running: None,
            _directory: directory,
        };
        result.start();
        result
    }
    fn start(&mut self) {
        let args: Vec<OsString> = vec![
            "--home".into(),
            self.home.path().into(),
            "--control-socket".into(),
            self.control.clone().into(),
            "--listen".into(),
            "127.0.0.1:0".into(),
            "--advertise".into(),
            "validator.test:8776".into(),
            "--max-validators".into(),
            "4".into(),
            "--external-inbound-reserve".into(),
            "0".into(),
            "--status-listen".into(),
            "127.0.0.1:0".into(),
        ];
        let (signal, receiver) = mpsc::sync_channel(1);
        let prepared =
            PreparedSidecar::prepare(Options::parse_from(args).unwrap(), receiver).unwrap();
        self.node_id = prepared.node_id();
        self.status = prepared.status_address();
        self.signal = Some(signal);
        self.running = Some(thread::spawn(move || prepared.run()));
    }
    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            self.node.clone().shutdown().unwrap();
            self.signal.take();
            running.join().unwrap().unwrap();
        }
    }
    fn journal(&self) -> PathBuf {
        self.home.node().join("outbe-governments")
    }
    fn partial(&self, index: usize) -> bool {
        let Ok(bytes) = std::fs::read(self.journal().join("current")) else {
            return false;
        };
        let body = PublicationBody::from_bytes(&bytes).unwrap();
        self.journal()
            .join(hex::encode(body.receipt_hash()))
            .join(format!("partial-0-{index}"))
            .exists()
    }
    fn available(&self, rid: RepoId) -> bool {
        let mut stream = TcpStream::connect_timeout(&self.status, HANDLE_DEADLINE).unwrap();
        stream.set_read_timeout(Some(HANDLE_DEADLINE)).unwrap();
        stream.write_all(format!("GET /v1/repositories/{} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n", hex::encode(rid.into_sha1().unwrap())).as_bytes()).unwrap();
        let mut result = String::new();
        stream.read_to_string(&mut result).unwrap();
        result.starts_with("HTTP/1.1 200") && result.contains("\"available\":true")
    }
}
impl Drop for Sidecar {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MergeCase {
    FastForward,
    Clean,
    Conflict,
    TargetMoved,
}
impl MergeCase {
    fn unmerged_reason(self) -> Option<&'static str> {
        match self {
            Self::Conflict => Some("Conflict"),
            Self::TargetMoved => Some("TargetMoved"),
            Self::FastForward | Self::Clean => None,
        }
    }
}

async fn exercise_patch_vote(
    sidecars: &mut [Sidecar],
    reader: &RethSnapshotReader<FinalizedProvider>,
    provider: &FinalizedProvider,
    state: &mut HashMapStorageProvider,
    mailboxes: &[RepositoryPublicationMailbox],
    height: &mut u64,
    case: MergeCase,
) {
    let root = reader.read_current_publication_receipt(1).unwrap().unwrap();
    let rid = RepoId::from(git::Oid::from_sha1(root.receipt().body.repo_id.0));
    let sequence = state
        .enter(|s| CodeRegistry::new(s).publication_sequence.read())
        .unwrap();
    let previous = reader
        .read_current_publication_receipt(sequence)
        .unwrap()
        .unwrap();
    let review_sequence = sequence + 1;
    let final_sequence = sequence
        + if case.unmerged_reason().is_some() {
            1
        } else {
            2
        };
    let author = SigningKey::from_seed(Seed::new([42; 32]));
    sidecars[1].stop();
    let (patch_id, revision, target, expected_target, head) = {
        let store = ReviewPackageStore::open(sidecars[1].journal().join("packages")).unwrap();
        let repo = store
            .open_checkpoint_repository(
                reader,
                &previous,
                &sidecars[1].home.storage().join(rid.canonical()),
                git::UserInfo {
                    alias: radicle::node::Alias::new("Author"),
                    key: *author.public_key(),
                },
            )
            .unwrap();
        let target = git::Oid::from(repo.raw().head().unwrap().target().unwrap());
        let initial = store
            .load_checkpoint_manifest(&root)
            .unwrap()
            .owner_refs()
            .unwrap()
            .head(git::fmt::refname!("main"))
            .unwrap();
        let base = if matches!(case, MergeCase::FastForward | MergeCase::TargetMoved) {
            target
        } else {
            initial
        };
        let expected_target = if case == MergeCase::TargetMoved {
            initial
        } else {
            target
        };
        if case == MergeCase::TargetMoved {
            assert_ne!(target, expected_target);
        }
        let content: &[u8] = if case == MergeCase::Conflict {
            b"Conflicting patch\n"
        } else {
            b"Governments patch\n"
        };
        let blob = repo.raw().blob(content).unwrap();
        let parent = repo.raw().find_commit(base.into()).unwrap();
        let mut tree = repo
            .raw()
            .treebuilder(Some(&parent.tree().unwrap()))
            .unwrap();
        let file = if case == MergeCase::Clean {
            "second.txt"
        } else if case == MergeCase::TargetMoved {
            "third.txt"
        } else {
            "proposal.txt"
        };
        tree.insert(file, blob, 0o100644).unwrap();
        let tree = repo.raw().find_tree(tree.write().unwrap()).unwrap();
        let metadata = git::raw::Signature::new(
            "Author",
            "author@example.test",
            &git::raw::Time::new(1_700_000_010, 0),
        )
        .unwrap();
        let head: git::Oid = repo
            .raw()
            .commit(
                None,
                &metadata,
                &metadata,
                "Proposed change\n",
                &tree,
                &[&parent],
            )
            .unwrap()
            .into();
        let mut patches = Patches::open(&repo, WriteAs::new(&author)).unwrap();
        let mut cache = cob::cache::NoCache;
        let patch = patches
            .create(
                cob::Title::new("Network proposal").unwrap(),
                "Exact revision",
                patch::MergeTarget::default(),
                base,
                head,
                &[],
                &mut cache,
            )
            .unwrap();
        let id = *patch.id();
        let revision = patch.latest().0;
        repo.sign_refs(&author).unwrap();
        (id, revision, target, expected_target, head)
    };
    for sidecar in sidecars.iter_mut() {
        if sidecar.running.is_none() {
            sidecar.start();
        }
    }
    let deadline = Instant::now() + HANDLE_DEADLINE * 4;
    while !sidecars[1].available(rid) {
        assert!(
            Instant::now() < deadline,
            "author source did not restore accepted policy"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let source = sidecars[1].node_id;
    let address: radicle::node::Address = sidecars[1].node.listen_addrs().unwrap()[0].into();
    for (index, sidecar) in sidecars.iter_mut().enumerate() {
        if index == 1 {
            continue;
        }
        sidecar
            .node
            .connect(
                source,
                address.clone(),
                radicle::node::ConnectOptions {
                    persistent: false,
                    timeout: HANDLE_DEADLINE,
                },
            )
            .unwrap();
        let deadline = Instant::now() + HANDLE_DEADLINE * 4;
        let fetch = loop {
            let fetch = sidecar
                .node
                .fetch(rid, source, HANDLE_DEADLINE, None)
                .unwrap();
            if fetch.is_success() {
                break fetch;
            }
            assert!(
                Instant::now() < deadline,
                "pre-Vote Ed author patch fetch: {fetch:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert!(fetch
            .success()
            .unwrap()
            .1
            .contains(&(*author.public_key()).into()));
    }
    assert!(reader
        .read_current_publication_receipt(review_sequence)
        .unwrap()
        .is_none());
    static TARGET: CodePatchVoteTarget = CodePatchVoteTarget;
    static HANDLERS: &[&dyn VoteTarget] = &[&TARGET];
    static REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(HANDLERS);
    *height += 1;
    state.set_block_number(*height);
    let create = IVote::createProposalWithVotingWindowCall {
        targetModule: CODE_REGISTRY_ADDRESS,
        payload: String::from_utf8(
            PatchVotePayloadV1::accept(
                rid.into_sha1().unwrap().into(),
                (*patch_id).into_sha1().unwrap().into(),
                revision.into_inner().into_sha1().unwrap().into(),
                expected_target.into_sha1().unwrap().into(),
            )
            .encode()
            .unwrap(),
        )
        .unwrap(),
        votingWindowBlocks: 2,
    };
    let result = state
        .enter(|s| {
            dispatch_with_handlers(
                s,
                &create.abi_encode(),
                Address::repeat_byte(1),
                U256::ZERO,
                &REGISTRY,
            )
        })
        .unwrap();
    let proposal = IVote::createProposalWithVotingWindowCall::abi_decode_returns(&result).unwrap();
    provider.finalize(*height, state);
    *height += 1;
    state.set_block_number(*height);
    for address in 1..=3 {
        let cast = IVote::castVoteCall {
            proposalId: proposal,
            approve: true,
        };
        state
            .enter(|s| {
                dispatch_with_handlers(
                    s,
                    &cast.abi_encode(),
                    Address::repeat_byte(address),
                    U256::ZERO,
                    &REGISTRY,
                )
            })
            .unwrap();
    }
    provider.finalize(*height, state);
    let chain_id = state.chain_id();
    for _ in 0..2 {
        *height += 1;
        state.set_block_number(*height);
        state
            .enter(|s| {
                let ctx = BlockRuntimeContext::new(
                    BlockContext::empty_for_tests(*height, 1_700_000_000 + *height, chain_id),
                    s.clone(),
                );
                Vote::new(s).process_begin_block(&ctx, &REGISTRY)
            })
            .unwrap();
        provider.finalize(*height, state);
    }
    let page = reader
        .read_finalized_vote_page(*height, 0)
        .unwrap()
        .unwrap();
    assert_eq!(
        page.proposals,
        [proposal],
        "worker consumes real finalized Vote event"
    );
    let approval_height = *height;
    let deadline = Instant::now() + HANDLE_DEADLINE * 16;
    let mut accounted = BTreeSet::new();
    loop {
        for mailbox in mailboxes {
            let Some(batch) = mailbox.ready_prefix(MAX_PUBLICATION_BATCH_LEN) else {
                continue;
            };
            if batch.body.proposal_id != proposal
                || batch
                    .attestations
                    .iter()
                    .all(|a| accounted.contains(&(batch.body.receipt_hash(), a.participant_index)))
            {
                continue;
            }
            *height += 1;
            state.set_block_number(*height);
            state
                .enter(|s| CodeRegistry::new(s).apply_publication_batch(&batch))
                .unwrap();
            accounted.extend(
                batch
                    .attestations
                    .iter()
                    .map(|a| (batch.body.receipt_hash(), a.participant_index)),
            );
            provider.finalize(*height, state);
        }
        let merged = reader
            .read_current_publication_receipt(final_sequence)
            .unwrap();
        if let Some(merged) = merged {
            assert_eq!(
                merged.receipt().body.action,
                if case.unmerged_reason().is_some() {
                    outbe_primitives::radicle_publication::PublicationAction::Review
                } else {
                    outbe_primitives::radicle_publication::PublicationAction::Merge
                }
            );
            assert_eq!(merged.receipt().body.proposal_id, proposal);
            if sidecars.iter().all(|s| {
                std::fs::read(s.journal().join("latest-installed")).is_ok_and(|bytes| {
                    bytes.get(..8) == Some(final_sequence.to_be_bytes().as_slice())
                })
            }) && (case.unmerged_reason().is_none()
                || sidecars.iter().all(|s| {
                    s.journal()
                        .join("approved-unmerged")
                        .join(format!("{approval_height:020}-{proposal:064x}"))
                        .is_file()
                }))
            {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "automatic Vote → Review → Merge did not complete; reviewed={:?}, merged={:?}",
            reader
                .read_current_publication_receipt(review_sequence)
                .unwrap(),
            reader
                .read_current_publication_receipt(final_sequence)
                .unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let review = reader
        .read_current_publication_receipt(review_sequence)
        .unwrap()
        .unwrap();
    assert_eq!(
        review.receipt().body.action,
        outbe_primitives::radicle_publication::PublicationAction::Review
    );
    let source = sidecars[1].node_id;
    let fetched = sidecars[0]
        .node
        .fetch(rid, source, HANDLE_DEADLINE, None)
        .unwrap();
    assert!(
        fetched.is_success(),
        "native accepted-Merge fetch: {fetched:?}"
    );
    for sidecar in sidecars.iter_mut() {
        sidecar.stop();
    }
    for sidecar in sidecars.iter() {
        let store = ReviewPackageStore::open(sidecar.journal().join("packages")).unwrap();
        let checkpoint = reader
            .read_current_publication_receipt(final_sequence)
            .unwrap()
            .unwrap();
        let repo = store
            .open_checkpoint_repository(
                reader,
                &checkpoint,
                &sidecar.home.storage().join(rid.canonical()),
                git::UserInfo {
                    alias: radicle::node::Alias::new("Verifier"),
                    key: *author.public_key(),
                },
            )
            .unwrap();
        let actual = git::Oid::from(repo.raw().head().unwrap().target().unwrap());
        match case {
            MergeCase::FastForward => assert_eq!(actual, head),
            MergeCase::Conflict | MergeCase::TargetMoved => {
                assert_eq!(actual, target);
                assert_eq!(
                    std::fs::read(
                        sidecar
                            .journal()
                            .join("approved-unmerged")
                            .join(format!("{approval_height:020}-{proposal:064x}"))
                    )
                    .unwrap(),
                    case.unmerged_reason().unwrap().as_bytes()
                );
            }
            MergeCase::Clean => {
                let commit = repo.raw().find_commit(actual.into()).unwrap();
                assert_eq!(
                    commit.parent_ids().collect::<Vec<_>>(),
                    [target.into(), head.into()]
                );
                let tree = commit.tree().unwrap();
                for name in ["proposal.txt", "second.txt"] {
                    let entry = tree.get_name(name).unwrap();
                    assert_eq!(
                        repo.raw().find_blob(entry.id()).unwrap().content(),
                        b"Governments patch\n"
                    );
                }
            }
        }
        let patches = Patches::open(&repo, WriteAs::new(&author)).unwrap();
        let merged = patches.get(&patch_id).unwrap().unwrap();
        assert_eq!(merged.is_merged(), case.unmerged_reason().is_none());
        if case.unmerged_reason().is_some() {
            assert!(merged.is_open());
        }
        let owner =
            radicle::crypto::BlsPublicKey::from_bytes(root.receipt().body.owner_bls_public.0)
                .unwrap();
        assert_eq!(
            merged
                .revision(&revision)
                .unwrap()
                .review_by(&owner.into())
                .unwrap()
                .verdict(),
            Some(patch::Verdict::Accept)
        );
    }
}

fn owner_refs(
    sidecar: &Sidecar,
    rid: RepoId,
    owner: radicle::crypto::RepositoryKey,
) -> BTreeMap<String, String> {
    let repo =
        git::raw::Repository::open_bare(sidecar.home.storage().join(rid.canonical())).unwrap();
    repo.references_glob(&format!("refs/namespaces/{owner}/refs/*"))
        .unwrap()
        .map(|reference| {
            let reference = reference.unwrap();
            (
                reference.name().unwrap().to_owned(),
                reference
                    .symbolic_target()
                    .unwrap()
                    .map(str::to_owned)
                    .unwrap_or_else(|| reference.target().unwrap().to_string()),
            )
        })
        .collect()
}

async fn exercise_material_binding(
    sidecars: &mut [Sidecar],
    reader: &RethSnapshotReader<FinalizedProvider>,
    provider: &FinalizedProvider,
    state: &mut HashMapStorageProvider,
    mailboxes: &[RepositoryPublicationMailbox],
    height: &mut u64,
) {
    use outbe_primitives::radicle_publication::PublicationAction;
    let sequence = state
        .enter(|s| CodeRegistry::new(s).publication_sequence.read())
        .unwrap();
    let before = reader
        .read_current_publication_receipt(sequence)
        .unwrap()
        .unwrap();
    let owner = radicle::crypto::BlsPublicKey::from_bytes(before.receipt().body.owner_bls_public.0)
        .unwrap()
        .into();
    let rid = git::Oid::from_sha1(before.receipt().body.repo_id.0).into();
    let native: Vec<_> = sidecars
        .iter()
        .map(|sidecar| owner_refs(sidecar, rid, owner))
        .collect();
    for sidecar in sidecars.iter_mut() {
        sidecar.start();
    }
    let next = sequence + 1;
    let mut accounted = BTreeSet::new();
    let deadline = Instant::now() + HANDLE_DEADLINE * 8;
    loop {
        for mailbox in mailboxes {
            let Some(batch) = mailbox.ready_prefix(MAX_PUBLICATION_BATCH_LEN) else {
                continue;
            };
            if batch.body.action != PublicationAction::BindMaterial
                || batch.body.sequence != next
                || batch
                    .attestations
                    .iter()
                    .all(|a| accounted.contains(&a.participant_index))
            {
                continue;
            }
            *height += 1;
            state.set_block_number(*height);
            state
                .enter(|s| CodeRegistry::new(s).apply_publication_batch(&batch))
                .unwrap();
            accounted.extend(batch.attestations.iter().map(|a| a.participant_index));
            provider.finalize(*height, state);
        }
        if let Some(receipt) = reader.read_current_publication_receipt(next).unwrap() {
            assert_eq!(
                receipt.receipt().body.action,
                PublicationAction::BindMaterial
            );
            assert_eq!(
                receipt.receipt().body.owner_bls_public,
                before.receipt().body.owner_bls_public
            );
            assert_ne!(
                receipt.receipt().body.material_hash,
                before.receipt().body.material_hash
            );
            assert_eq!(receipt.receipt().body.material_version, 8);
            if sidecars.iter().all(|s| {
                std::fs::read(s.journal().join("latest-installed"))
                    .is_ok_and(|bytes| bytes.get(..8) == Some(next.to_be_bytes().as_slice()))
            }) {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "automatic BindMaterial did not complete"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(accounted.len(), 3);
    for (sidecar, original) in sidecars.iter_mut().zip(native) {
        sidecar.stop();
        assert_eq!(owner_refs(sidecar, rid, owner), original);
        let store = ReviewPackageStore::open(sidecar.journal().join("packages")).unwrap();
        let bound = reader
            .read_current_publication_receipt(next)
            .unwrap()
            .unwrap();
        let manifest = store.load_checkpoint_manifest(&bound).unwrap();
        assert!(manifest.objects().is_empty());
        assert!(manifest.subjects().is_empty());
        assert!(manifest.refs().is_empty());
        assert_eq!(
            manifest.owner_refs().unwrap(),
            store
                .load_checkpoint_manifest(&before)
                .unwrap()
                .owner_refs()
                .unwrap()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn automatic_bootstrap_vote_review_and_merge_with_one_offline_member_and_restarts() {
    let _ = log::set_logger(&TestLogger);
    log::set_max_level(log::LevelFilter::Debug);
    let chain = ChainIdentity {
        chain_id: 2026,
        genesis_hash: B256::repeat_byte(7),
    };
    let mut keys: Vec<_> = (101..105).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|k| k.public_key());
    let dkg =
        bootstrap_dkg_for_participants(keys.iter().map(|k| k.public_key()).try_collect().unwrap())
            .unwrap();
    let committee = CommitteeSnapshot {
        committee: keys
            .iter()
            .enumerate()
            .map(|(i, key)| CommitteeEntry {
                address: Address::repeat_byte(i as u8 + 1),
                consensus_pubkey: key.public_key().encode().as_ref().try_into().unwrap(),
            })
            .collect(),
        vrf_material_version: 7,
        vrf_group_public_key_bytes: dkg.polynomial.public().encode().to_vec(),
        vrf_public_polynomial_hash: outbe_consensus::dkg_manager::public_polynomial_hash(
            &dkg.polynomial,
        ),
    };
    let mut state =
        HashMapStorageProvider::new_with_chain_identity(chain.chain_id, chain.genesis_hash);
    state.set_block_number(1);
    state.enter(|s| {
        let mut validators = ValidatorSet::new(s.clone());
        validators
            .config_owner
            .write(Address::repeat_byte(99))
            .unwrap();
        validators.set_config_max_validators(4).unwrap();
        let minimum = U256::from(1_000u64);
        validators
            .test_seed_authoritative_min_stake(minimum)
            .unwrap();
        for member in &committee.committee {
            validators
                .test_register_validator_without_pop(member.address, &member.consensus_pubkey)
                .unwrap();
            validators
                .test_seed_authoritative_bonded(member.address, minimum)
                .unwrap();
            validators
                .activate_validator_via_boundary_for_test(member.address)
                .unwrap();
        }
        validators.advance_epoch(1001, 1).unwrap();
        validators.advance_epoch(1002, 2).unwrap();
        write_committee_snapshot(s, 2, &committee).unwrap();
    });
    let genesis: serde_json::Value = serde_json::from_str(include_str!(
        "../../coderegistry/tests/fixtures/governments-genesis-storage.json"
    ))
    .unwrap();
    for (slot, value) in genesis["storage"].as_object().unwrap() {
        state.storage.insert(
            (CODE_REGISTRY_ADDRESS, slot.parse().unwrap()),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    let provider = FinalizedProvider::default();
    provider.finalize(1, &mut state);
    provider.finalize(2, &mut state);
    provider.finalize(3, &mut state);
    let reader = Arc::new(RethSnapshotReader::new(
        provider.clone(),
        chain.chain_id,
        chain.genesis_hash,
    ));
    let mut sidecars: Vec<_> = (71..74).map(Sidecar::new).collect();
    let mut routes = BTreeMap::new();
    let mut receivers = Vec::new();
    for key in &keys[..3] {
        let (sender, receiver) = async_mpsc::unbounded_channel();
        routes.insert(key.public_key(), sender);
        receivers.push(receiver);
    }
    let routes = Arc::new(routes);
    let mut services = Vec::new();
    let mut actors = Vec::new();
    let mut mailboxes = Vec::new();
    let mut signers = Vec::new();
    let mut delayed = None;
    for (index, receiver) in receivers.into_iter().enumerate() {
        let member = &committee.committee[index];
        let node_id = sidecars[index].node_id.into_inner();
        let (_, status) = RadicleStatusChannel::enabled(member.address, node_id);
        let (service, resolver, _) = EndpointNetwork::build(chain, status);
        let mailbox = RepositoryPublicationMailbox::default();
        let mut service = service
            .with_repository_authority_reader(reader.clone())
            .with_repository_publication_mailbox(mailbox.clone());
        let publisher = service.take_repository_material_publisher().unwrap();
        publisher.publish(2, 7, Some(&dkg.output));
        let signer =
            VrfMaterialProvider::new(7, dkg.polynomial.clone(), Some(dkg.shares[index].clone()));
        publisher.install_local_signer(signer.clone());
        signers.push(signer);
        let (_, local) = LocalEndpointIdentityChannel::create(LocalEndpointIdentity {
            validator: member.address,
            node_id,
            addresses: vec![EndpointAddress::dns("validator.test", 8776).unwrap()],
        });
        let run = service.run(
            EndpointTransport {
                sender: RoutedSender {
                    sender: keys[index].public_key(),
                    routes: routes.clone(),
                    recipient: None,
                },
                receiver: RoutedReceiver(receiver),
            },
            EndpointSigningIdentity {
                signer: keys[index].clone(),
                local,
            },
        );
        actors.push(if index < 2 {
            Some(tokio::spawn(run))
        } else {
            delayed = Some(run);
            None
        });
        let store = ReviewPackageStore::open(sidecars[index].home.node().join("node-publications"))
            .unwrap();
        let server = LocalReceiptServer::bind(&sidecars[index].control, reader.clone())
            .unwrap()
            .with_native_node_id(node_id)
            .with_authority_reader(reader.clone())
            .with_material_resolver(resolver.clone())
            .with_publication_store(store);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(server.run(async {
            let _ = stopped.await;
        }));
        services.push((resolver, publisher, stop, task));
        mailboxes.push(mailbox);
    }
    // Two online shares cannot publish. Restart a collector before enabling the third signer.
    let deadline = Instant::now() + HANDLE_DEADLINE * 4;
    while !sidecars[0].partial(0) || !sidecars[1].partial(1) {
        assert!(
            Instant::now() < deadline,
            "first two collectors did not journal their shares"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(reader
        .read_current_publication_receipt(1)
        .unwrap()
        .is_none());
    assert!(mailboxes
        .iter()
        .all(|m| m.ready_prefix(MAX_PUBLICATION_BATCH_LEN).is_none()));
    sidecars[0].stop();
    sidecars[0].start();
    actors[2] = Some(tokio::spawn(delayed.take().unwrap()));

    let mut height = 3;
    let mut accepted = None;
    let mut accounted = BTreeSet::new();
    // Three native signing phases plus registry and install, with their existing SDK deadlines.
    let deadline = Instant::now() + HANDLE_DEADLINE * 12;
    loop {
        if accepted.is_none() {
            for mailbox in &mailboxes {
                let Some(batch) = mailbox.ready_prefix(MAX_PUBLICATION_BATCH_LEN) else {
                    continue;
                };
                if batch
                    .attestations
                    .iter()
                    .all(|a| accounted.contains(&a.participant_index))
                {
                    continue;
                }
                height += 1;
                state.set_block_number(height);
                let result = state
                    .enter(|s| CodeRegistry::new(s).apply_publication_batch(&batch))
                    .unwrap();
                accounted.extend(batch.attestations.iter().map(|a| a.participant_index));
                provider.finalize(height, &mut state);
                if let Some(receipt) = result.receipt {
                    accepted = Some(receipt);
                    break;
                }
            }
        }
        if sidecars
            .iter()
            .all(|s| s.journal().join("announced").is_file())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "automatic bootstrap did not finish; accepted={accepted:?}, journals={:?}",
            sidecars
                .iter()
                .map(|s| std::fs::read_dir(s.journal())
                    .map(|e| e.map(|e| e.unwrap().file_name()).collect::<Vec<_>>()))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let accepted = accepted.unwrap();
    assert_eq!(accounted.len(), 3);
    assert!(accounted.iter().all(|i| *i < 3));
    let rid = RepoId::from(git::Oid::from_sha1(accepted.body.repo_id.0));
    let mut heads = Vec::new();
    for sidecar in &sidecars {
        assert!(sidecar.available(rid));
        let repo =
            git::raw::Repository::open_bare(sidecar.home.storage().join(rid.canonical())).unwrap();
        heads.push(repo.head().unwrap().target().unwrap());
        assert_eq!(
            std::fs::read(sidecar.journal().join("installed")).unwrap(),
            accepted.receipt_hash.as_slice()
        );
    }
    assert!(heads.windows(2).all(|pair| pair[0] == pair[1]));
    let source = sidecars[1].node_id;
    let address = sidecars[1].node.listen_addrs().unwrap()[0].into();
    assert!(matches!(
        sidecars[0]
            .node
            .connect(
                source,
                address,
                radicle::node::ConnectOptions {
                    persistent: false,
                    timeout: HANDLE_DEADLINE,
                }
            )
            .unwrap(),
        radicle::node::ConnectResult::Connected
    ));
    let fetch = sidecars[0]
        .node
        .fetch(rid, source, HANDLE_DEADLINE, None)
        .unwrap();
    assert!(
        fetch.is_success(),
        "native accepted-bootstrap fetch failed: {fetch:?}"
    );
    let owner = radicle::crypto::RepositoryKey::from(
        radicle::crypto::BlsPublicKey::from_bytes(accepted.body.owner_bls_public.0).unwrap(),
    );
    assert!(fetch.success().unwrap().1.contains(&owner));
    sidecars[0].stop();
    sidecars[0].start();
    let deadline = Instant::now() + HANDLE_DEADLINE * 4;
    while !sidecars[0].available(rid) {
        assert!(
            Instant::now() < deadline,
            "accepted bootstrap was not restored after restart"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let client = LocalReceiptClient::connect(&sidecars[0].control)
        .await
        .unwrap();
    assert_eq!(client.read(1).await.unwrap().unwrap().receipt(), &accepted);
    for case in [
        MergeCase::FastForward,
        MergeCase::Clean,
        MergeCase::Conflict,
        MergeCase::TargetMoved,
    ] {
        exercise_patch_vote(
            &mut sidecars,
            &reader,
            &provider,
            &mut state,
            &mailboxes,
            &mut height,
            case,
        )
        .await;
        if case == MergeCase::FastForward {
            let refreshed = support::reshare(&keys, &dkg);
            let mut committee = committee.clone();
            committee.vrf_material_version = 8;
            committee.vrf_public_polynomial_hash =
                outbe_consensus::dkg_manager::public_polynomial_hash(&refreshed.polynomial);
            height += 1;
            state.set_block_number(height);
            state
                .enter(|s| write_committee_snapshot(s, 2, &committee))
                .unwrap();
            provider.finalize(height, &mut state);
            for (index, (_, publisher, _, _)) in services.iter().enumerate() {
                signers[index].activate(
                    8,
                    refreshed.polynomial.clone(),
                    Some(refreshed.shares[index].clone()),
                );
                publisher.publish(2, 8, Some(&refreshed.output));
            }
            exercise_material_binding(
                &mut sidecars,
                &reader,
                &provider,
                &mut state,
                &mailboxes,
                &mut height,
            )
            .await;
        }
    }
    // History survives material rotation without either old signing transcripts or local selection.
    let archive = sidecars[0].journal().join("packages");
    std::fs::remove_dir_all(archive.join("slots")).unwrap();
    for package in std::fs::read_dir(archive.join("packages")).unwrap() {
        let package = package.unwrap().path();
        for name in ["witnesses", "ready"] {
            let path = package.join(name);
            if path.exists() {
                std::fs::remove_dir_all(path).unwrap();
            }
        }
        let content = package.join("initial-content.manifest");
        if content.exists() {
            std::fs::remove_file(content).unwrap();
        }
    }
    services[0].1.publish(2, 8, None);
    sidecars[0].start();
    let deadline = Instant::now() + HANDLE_DEADLINE * 4;
    while !sidecars[0].available(rid) {
        assert!(
            Instant::now() < deadline,
            "accepted Merge was not restored without current material"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        std::fs::read(sidecars[0].journal().join("latest-installed")).unwrap()[..8],
        8u64.to_be_bytes(),
    );
    sidecars[1].start();
    let source = sidecars[1].node_id;
    let address = sidecars[1].node.listen_addrs().unwrap()[0].into();
    sidecars[0]
        .node
        .connect(
            source,
            address,
            radicle::node::ConnectOptions {
                persistent: false,
                timeout: HANDLE_DEADLINE,
            },
        )
        .unwrap();
    let deadline = Instant::now() + HANDLE_DEADLINE * 4;
    loop {
        let fetch = sidecars[0]
            .node
            .fetch(rid, source, HANDLE_DEADLINE, None)
            .unwrap();
        if fetch.is_success() {
            assert!(fetch.success().unwrap().1.contains(&owner));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "accepted history fetch requires no current material: {fetch:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // An ordinary Radicle client has no Outbe reader, signing share or receipt archive.
    let mut ordinary = Sidecar::new(75);
    ordinary
        .node
        .seed(rid, radicle::node::policy::Scope::All)
        .unwrap();
    ordinary
        .node
        .connect(
            source,
            sidecars[1].node.listen_addrs().unwrap()[0].into(),
            radicle::node::ConnectOptions {
                persistent: false,
                timeout: HANDLE_DEADLINE,
            },
        )
        .unwrap();
    let fetched = ordinary
        .node
        .fetch(rid, source, HANDLE_DEADLINE, None)
        .unwrap();
    assert!(fetched.is_success(), "ordinary BLS clone: {fetched:?}");
    assert!(fetched.success().unwrap().1.contains(&owner));
    assert!(!ordinary.journal().join("packages/packages").exists());
    {
        use radicle::storage::{
            ReadRepository as _, RemoteRepository as _, ValidateRepository as _,
        };
        let repository = radicle::storage::git::Repository::open(
            ordinary.home.storage().join(rid.canonical()),
            rid,
        )
        .unwrap();
        let source_repository =
            git::raw::Repository::open_bare(sidecars[1].home.storage().join(rid.canonical()))
                .unwrap();
        assert_eq!(
            repository
                .identity_doc()
                .unwrap()
                .doc
                .delegates()
                .first()
                .as_key(),
            &owner
        );
        assert_eq!(
            repository.raw().head().unwrap().target(),
            source_repository.head().unwrap().target()
        );
        assert!(repository.validate().unwrap().is_empty());
        let patches = Patches::open(&repository, radicle::cob::store::access::ReadOnly).unwrap();
        for sequence in 2..=8 {
            let receipt = reader
                .read_current_publication_receipt(sequence)
                .unwrap()
                .unwrap();
            let body = &receipt.receipt().body;
            if !matches!(
                body.action,
                outbe_primitives::radicle_publication::PublicationAction::Review
                    | outbe_primitives::radicle_publication::PublicationAction::Merge
            ) {
                continue;
            }
            let patch_id = git::Oid::from_sha1(body.patch_id.0).into();
            let revision = git::Oid::from_sha1(body.revision_id.0).into();
            let patch = patches.get(&patch_id).unwrap().unwrap();
            assert_eq!(
                patch
                    .revision(&revision)
                    .unwrap()
                    .review_by(&owner)
                    .unwrap()
                    .verdict(),
                Some(patch::Verdict::Accept)
            );
            if body.action == outbe_primitives::radicle_publication::PublicationAction::Merge {
                assert!(patch.is_merged());
            }
        }
        assert_eq!(
            repository.remote(&owner).unwrap().refs.at,
            git::Oid::from(
                source_repository
                    .refname_to_id(&format!("refs/namespaces/{owner}/refs/rad/sigrefs"),)
                    .unwrap()
            )
        );
    }
    let fetched = ordinary
        .node
        .fetch(rid, source, HANDLE_DEADLINE, None)
        .unwrap();
    assert!(
        fetched.is_success(),
        "ordinary repeated BLS fetch: {fetched:?}"
    );
    ordinary.stop();

    // Receipts alone cannot authorize recovery without the trusted descriptor and approvals.
    struct ReceiptOnlyReader(Arc<RethSnapshotReader<FinalizedProvider>>);
    impl outbe_radicle::manager::RepositoryReceiptReader for ReceiptOnlyReader {
        fn read_current_publication_receipt(
            &self,
            sequence: u64,
        ) -> Result<
            Option<outbe_radicle::manager::FinalizedPublicationReceipt>,
            outbe_radicle::manager::ManagerError,
        > {
            self.0.read_current_publication_receipt(sequence)
        }
    }
    let mut fresh = Sidecar::new(74);
    let store_path = fresh.home.node().join("node-publications");
    let store = ReviewPackageStore::open(&store_path).unwrap();
    let server =
        LocalReceiptServer::bind(&fresh.control, Arc::new(ReceiptOnlyReader(reader.clone())))
            .unwrap()
            .with_authority_reader(reader.clone())
            .with_native_node_id(fresh.node_id.into_inner())
            .with_publication_store(store);
    let (stop_fresh, stopped) = oneshot::channel();
    let serving = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let deadline = Instant::now() + HANDLE_DEADLINE * 4;
    while radicle::storage::git::Repository::open(fresh.home.storage().join(rid.canonical()), rid)
        .is_err()
    {
        assert!(
            Instant::now() < deadline,
            "fresh collector did not read its accepted receipt"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // A direct reopen has no in-memory provider; the persisted owner pin must still deny access.
    {
        use radicle::storage::ReadRepository as _;
        let repository = radicle::storage::git::Repository::open(
            fresh.home.storage().join(rid.canonical()),
            rid,
        )
        .unwrap();
        let source_repository =
            git::raw::Repository::open_bare(sidecars[1].home.storage().join(rid.canonical()))
                .unwrap();
        let identity = source_repository
            .refname_to_id("refs/rad/id")
            .unwrap()
            .into();
        for kind in [
            radicle::crypto::SubjectKind::Identity,
            radicle::crypto::SubjectKind::Cob,
            radicle::crypto::SubjectKind::SignedRefs,
        ] {
            assert!(matches!(
                repository.authorize_entry(owner, identity, kind),
                Err(radicle::storage::publication::Error::Unavailable)
            ));
        }
    }
    fresh
        .node
        .seed(rid, radicle::node::policy::Scope::All)
        .unwrap();
    let address = sidecars[1].node.listen_addrs().unwrap()[0].into();
    fresh
        .node
        .connect(
            source,
            address,
            radicle::node::ConnectOptions {
                persistent: false,
                timeout: HANDLE_DEADLINE,
            },
        )
        .unwrap();
    let fetched = fresh
        .node
        .fetch(rid, source, HANDLE_DEADLINE, None)
        .unwrap();
    match fetched {
        // Native fetch cannot authorize the fetched identity without accepted recovery history.
        radicle::node::FetchResult::Failed { reason } => {
            assert_eq!(
                reason,
                format!("verification of rad/id for {source} failed")
            )
        }
        other => panic!("fresh receiver accepted owner entries without its package: {other:?}"),
    }
    let empty =
        git::raw::Repository::open_bare(fresh.home.storage().join(rid.canonical())).unwrap();
    assert!(empty.head().is_err());
    assert!(empty
        .references_glob(&format!("refs/namespaces/{owner}/refs/*"))
        .unwrap()
        .next()
        .is_none());
    drop(empty);
    stop_fresh.send(()).unwrap();
    serving.await.unwrap().unwrap();

    // Full finalized history permits reconstruction, but only through accepted-proof checks.
    let store = ReviewPackageStore::open(&store_path).unwrap();
    let server = LocalReceiptServer::bind(&fresh.control, reader.clone())
        .unwrap()
        .with_authority_reader(reader.clone())
        .with_native_node_id(fresh.node_id.into_inner())
        .with_publication_store(store);
    let (stop_fresh, stopped) = oneshot::channel();
    let serving = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let deadline = Instant::now() + HANDLE_DEADLINE * 4;
    loop {
        let fetched = fresh
            .node
            .fetch(rid, source, HANDLE_DEADLINE, None)
            .unwrap();
        if fetched.is_success() {
            assert!(fetched.success().unwrap().1.contains(&owner));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fresh receiver did not recover accepted history: {fetched:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    while !fresh.available(rid)
        || !std::fs::read(fresh.journal().join("latest-installed"))
            .is_ok_and(|bytes| bytes.get(..8) == Some(8u64.to_be_bytes().as_slice()))
    {
        assert!(
            Instant::now() < deadline,
            "fresh receiver did not install accepted history through sequence 8"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        owner_refs(&fresh, rid, owner),
        owner_refs(&sidecars[1], rid, owner)
    );
    fresh.stop();
    stop_fresh.send(()).unwrap();
    serving.await.unwrap().unwrap();
    {
        use radicle::storage::{
            ReadRepository as _, RemoteRepository as _, ValidateRepository as _,
        };
        let store = ReviewPackageStore::open(fresh.journal().join("packages")).unwrap();
        let checkpoint = reader.read_current_publication_receipt(8).unwrap().unwrap();
        let verifier = SigningKey::from_seed(Seed::new([76; 32]));
        let repository = store
            .open_checkpoint_repository(
                reader.as_ref(),
                &checkpoint,
                &fresh.home.storage().join(rid.canonical()),
                git::UserInfo {
                    alias: radicle::node::Alias::new("Verifier"),
                    key: *verifier.public_key(),
                },
            )
            .unwrap();
        for sequence in 1..=8 {
            let receipt = reader
                .read_current_publication_receipt(sequence)
                .unwrap()
                .unwrap();
            if matches!(
                receipt.receipt().body.action,
                outbe_primitives::radicle_publication::PublicationAction::Bootstrap
                    | outbe_primitives::radicle_publication::PublicationAction::Review
                    | outbe_primitives::radicle_publication::PublicationAction::Merge
            ) {
                assert!(store
                    .load_accepted(&receipt, &repository)
                    .unwrap()
                    .is_some());
            }
        }
        let source_repository =
            git::raw::Repository::open_bare(sidecars[1].home.storage().join(rid.canonical()))
                .unwrap();
        assert_eq!(
            repository
                .identity_doc()
                .unwrap()
                .doc
                .delegates()
                .first()
                .as_key(),
            &owner
        );
        assert_eq!(
            repository.raw().head().unwrap().target(),
            source_repository.head().unwrap().target()
        );
        assert_eq!(
            repository.remote(&owner).unwrap().refs.at,
            git::Oid::from(
                source_repository
                    .refname_to_id(&format!("refs/namespaces/{owner}/refs/rad/sigrefs"))
                    .unwrap()
            )
        );
        assert!(repository.validate().unwrap().is_empty());
    }
    for sidecar in &mut sidecars {
        sidecar.stop();
    }
    for ((resolver, _publisher, stop, task), actor) in services.into_iter().zip(actors) {
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
        resolver.shutdown().await.unwrap();
        actor.unwrap().await.unwrap().unwrap();
    }
}
