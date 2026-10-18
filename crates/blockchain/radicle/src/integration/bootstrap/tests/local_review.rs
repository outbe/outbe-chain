use super::*;
use crate::{
    endpoint::{ChainIdentity, EndpointAddress},
    integration::{
        local::{LocalReceiptClient, LocalReceiptServer},
        network::shutdown_tests::{NoSend, TestReceiver},
        publication::{
            review::prepare_review_publication,
            store::{review_slot_key, ReviewPackageStore},
        },
        review::prepare_patch_review_from_checkpoint,
        signing::witness::RepositoryWitnessBundle,
        EndpointNetwork, EndpointSigningIdentity, EndpointTransport, LocalEndpointIdentity,
        LocalEndpointIdentityChannel, RadicleStatusChannel,
    },
    manager::{
        FinalizedPatchApproval, FinalizedPatchSigningState, FinalizedPublicationReceipt,
        FinalizedRepositoryAuthority, ManagerError, RepositoryAuthorityReader,
        RepositoryReceiptReader,
    },
};
use commonware_cryptography::bls12381::{
    dkg::feldman_desmedt::Output, primitives::variant::MinSig,
};
use outbe_radicleregistry::{payload::PatchVotePayloadV1, PatchApprovalRecord};
use radicle::cob::{self, patch::Patches, store::access::WriteAs};
use std::os::unix::fs::PermissionsExt as _;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

struct Reader {
    state: Mutex<Option<FinalizedPatchSigningState>>,
    root: FinalizedPublicationReceipt,
    accepted: Mutex<Vec<FinalizedPublicationReceipt>>,
    approval: Mutex<Option<FinalizedPatchApproval>>,
}
impl RepositoryReceiptReader for Reader {
    fn read_current_patch_approval(
        &self,
        proposal: U256,
    ) -> Result<Option<FinalizedPatchApproval>, ManagerError> {
        Ok(self
            .approval
            .lock()
            .unwrap()
            .as_ref()
            .filter(|a| a.approval.proposal_id == proposal)
            .cloned())
    }
    fn read_current_publication_receipt(
        &self,
        sequence: u64,
    ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError> {
        Ok(if sequence == 1 {
            Some(self.root.clone())
        } else {
            self.accepted
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.receipt().body.sequence == sequence)
                .cloned()
        })
    }
}
impl RepositoryAuthorityReader for Reader {
    fn read_current_repository_authority(
        &self,
    ) -> Result<FinalizedRepositoryAuthority, ManagerError> {
        self.state
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.authority().clone())
            .ok_or_else(|| ManagerError::Snapshot("fixture unavailable".into()))
    }
    fn read_current_patch_signing_state(
        &self,
        proposal: U256,
    ) -> Result<FinalizedPatchSigningState, ManagerError> {
        self.state
            .lock()
            .unwrap()
            .as_ref()
            .filter(|s| s.approval().approval.proposal_id == proposal)
            .cloned()
            .ok_or_else(|| ManagerError::Snapshot("fixture unavailable".into()))
    }
}

pub(super) async fn assert_review_socket(
    repo: &Repository,
    archive: (ReviewPackageStore, &std::path::Path),
    mut root: FinalizedPublicationReceipt,
    material: &RepositorySigningMaterial,
    keys: &[bls12381::PrivateKey],
    providers: &[VrfMaterialProvider<MinSig>],
    output: &Output<MinSig, bls12381::PublicKey>,
) {
    let (store, store_path) = archive;
    let author = radicle::crypto::SigningKey::from_seed(radicle::crypto::Seed::new([42; 32]));
    let base = repo.head().unwrap().1;
    let blob = repo.raw().blob(b"proposed code").unwrap();
    let mut builder = repo.raw().treebuilder(None).unwrap();
    builder.insert("README", blob, 0o100644).unwrap();
    let tree = repo.raw().find_tree(builder.write().unwrap()).unwrap();
    let metadata = git::raw::Signature::new(
        "Author",
        "author@example.test",
        &git::raw::Time::new(1700000001, 0),
    )
    .unwrap();
    let parent = repo.raw().find_commit(base.into()).unwrap();
    let head: git::Oid = repo
        .raw()
        .commit(
            None,
            &metadata,
            &metadata,
            "Proposed code",
            &tree,
            &[&parent],
        )
        .unwrap()
        .into();
    let mut patches = Patches::open(repo, WriteAs::new(&author)).unwrap();
    let mut cache = cob::cache::NoCache;
    let mut patch = patches
        .create(
            cob::Title::new("Proposed patch").unwrap(),
            "Exact revision",
            cob::patch::MergeTarget::default(),
            base,
            head,
            &[],
            &mut cache,
        )
        .unwrap();
    let patch_id = *patch.id();
    let revision = patch.latest().0;
    let block = crate::manager::FinalizedBlock {
        number: 17,
        hash: B256::repeat_byte(17),
    };
    root.block = block;
    let mut authority = material.authority().clone();
    authority.block = block;
    let state = FinalizedPatchSigningState {
        authority: authority.clone(),
        approval: FinalizedPatchApproval {
            block,
            approval: PatchApprovalRecord {
                proposal_id: U256::from(5),
                payload: String::from_utf8(
                    PatchVotePayloadV1::accept(
                        repo.id().into_sha1().unwrap().into(),
                        (*patch_id).into_sha1().unwrap().into(),
                        revision.into_inner().into_sha1().unwrap().into(),
                        base.into_sha1().unwrap().into(),
                    )
                    .encode()
                    .unwrap(),
                )
                .unwrap(),
                approved_height: 16,
                approved_timestamp: 1700000016,
            },
        },
        checkpoint: root.clone(),
        material: root.clone(),
        stage: outbe_radicleregistry::schema::PATCH_EXECUTION_PENDING,
        anchor: crate::manager::FinalizedBlock {
            number: 16,
            hash: B256::repeat_byte(16),
        },
    };
    let wrong_signature = BlsEnvelope::from_hex(
        &store.load_checkpoint_manifest(&root).unwrap().subjects()[0].envelope,
    )
    .unwrap();
    let reader = Arc::new(Reader {
        state: Mutex::new(Some(state.clone())),
        root,
        accepted: Mutex::new(vec![]),
        approval: Mutex::new(None),
    });
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let control = directory.path().join("control");
    let chain = ChainIdentity {
        chain_id: authority.chain_id(),
        genesis_hash: authority.genesis_hash(),
    };
    let node_id = author.public_key().into_inner();
    let validator = authority.committee().committee[0].address;
    let (_, status) = RadicleStatusChannel::enabled(validator, node_id);
    let (service, resolver, _) = EndpointNetwork::build(chain, status);
    let mailbox = outbe_primitives::radicle_publication::RepositoryPublicationMailbox::default();
    let mut service = service
        .with_repository_authority_reader(reader.clone())
        .with_repository_publication_mailbox(mailbox.clone());
    let publisher = service.take_repository_material_publisher().unwrap();
    publisher.publish(authority.epoch(), 7, Some(output));
    let (_, local) = LocalEndpointIdentityChannel::create(LocalEndpointIdentity {
        validator,
        node_id,
        addresses: vec![EndpointAddress::dns("local.example", 8776).unwrap()],
    });
    let (_incoming, receiver) = mpsc::unbounded_channel();
    let actor = tokio::spawn(service.run(
        EndpointTransport {
            sender: NoSend,
            receiver: TestReceiver(receiver),
        },
        EndpointSigningIdentity {
            signer: keys[0].clone(),
            local,
        },
    ));
    let server = LocalReceiptServer::bind(&control, reader.clone())
        .unwrap()
        .with_authority_reader(reader.clone())
        .with_material_resolver(resolver.clone())
        .with_native_node_id(node_id)
        .with_publication_store(store);
    let (stop, stopping) = oneshot::channel();
    let socket = tokio::spawn(server.run(async {
        let _ = stopping.await;
    }));
    let client = LocalReceiptClient::new(&control, chain);
    let context = client
        .read_patch_preparation_context(U256::from(5))
        .await
        .unwrap();
    let signing = context
        .material
        .review_context(context.snapshot.checkpoint().receipt().receipt_hash.0);
    let prepare = || {
        prepare_patch_review_from_checkpoint(
            repo,
            context.state.approval(),
            &context.snapshot,
            &signing,
        )
        .unwrap()
    };
    let job = prepare();
    let later_revision = patch
        .update("Unapproved later revision", base, head)
        .unwrap();
    assert_ne!(later_revision, revision);
    assert!(client.sign_review(&context, repo, &job).await.is_err());
    publisher.install_local_signer(providers[0].clone());
    let own = client.sign_review(&context, repo, &job).await.unwrap();
    assert_eq!(own.participant_index(), 0);
    let contributions: Vec<_> = (0..3)
        .map(|i| {
            (
                keys[i].public_key(),
                if i == 0 {
                    own.clone()
                } else {
                    context
                        .material
                        .sign_current_review(
                            &providers[i],
                            &job,
                            &state,
                            authority.committee().committee[i].address,
                            &keys[i],
                        )
                        .unwrap()
                },
            )
        })
        .collect();
    let public = VrfMaterialProvider::new(7, output.public().clone(), None);
    let signature = context
        .material
        .recover_review(&public, &job, &contributions)
        .unwrap();
    let (_, refs) = job.attach(repo, &signature).unwrap();
    let partial = client
        .sign_review_refs(&context, repo, &refs, &signature)
        .await
        .unwrap();
    assert_eq!(partial.participant_index(), 0);
    let refs_contributions: Vec<_> = (0..3)
        .map(|i| {
            (
                keys[i].public_key(),
                if i == 0 {
                    partial.clone()
                } else {
                    context
                        .material
                        .sign_current_review_refs(
                            &providers[i],
                            &refs,
                            &state,
                            authority.committee().committee[i].address,
                            &keys[i],
                        )
                        .unwrap()
                },
            )
        })
        .collect();
    let refs_signature = context
        .material
        .recover_review_refs(&public, &refs, &refs_contributions)
        .unwrap();
    let witness = RepositoryWitnessBundle::new(
        context.material.canonical_output(),
        vec![
            contributions.iter().map(|(_, p)| p.clone()).collect(),
            refs_contributions.iter().map(|(_, p)| p.clone()).collect(),
        ],
    )
    .unwrap();
    let package = prepare_review_publication(
        repo,
        &context.state,
        &context.snapshot,
        &context.material,
        [&signature, &refs_signature],
        &witness,
    )
    .unwrap();
    let slot = store_path
        .join("slots")
        .join(hex::encode(review_slot_key(package.body())));
    assert!(!slot.exists());
    let short = RepositoryWitnessBundle::new(
        context.material.canonical_output(),
        vec![
            contributions[..2].iter().map(|(_, p)| p.clone()).collect(),
            refs_contributions.iter().map(|(_, p)| p.clone()).collect(),
        ],
    )
    .unwrap();
    assert!(client
        .attest_review_publication(&context, repo, &package, &short)
        .await
        .is_err());
    assert!(!slot.exists());
    use outbe_primitives::radicle_publication::{
        PublicationAttestation, PublicationBatch, PUBLICATION_BODY_LEN, PUBLICATION_NAMESPACE,
    };
    let batch = PublicationBatch {
        body: package.body().clone(),
        attestations: (0..3)
            .map(|i| PublicationAttestation {
                participant_index: i as u32,
                signature: alloy_primitives::FixedBytes::from_slice(
                    keys[i]
                        .sign(PUBLICATION_NAMESPACE, &package.body().to_bytes())
                        .encode()
                        .as_ref(),
                ),
            })
            .collect(),
    };
    let mut bad_last = batch.clone();
    bad_last.attestations[2].signature.0[0] ^= 1;
    assert!(client
        .queue_review_publication(repo, &package, &witness, &bad_last)
        .await
        .is_err());
    assert!(!slot.exists());
    assert!(mailbox.ready_prefix(usize::MAX).is_none());
    let own_attestation = client
        .attest_review_publication(&context, repo, &package, &witness)
        .await
        .unwrap();
    assert_eq!(own_attestation, batch.attestations[0]);
    assert!(slot.is_file());
    assert_eq!(
        client
            .attest_review_publication(&context, repo, &package, &witness)
            .await
            .unwrap(),
        own_attestation
    );
    client
        .queue_review_publication(repo, &package, &witness, &batch)
        .await
        .unwrap();
    assert_eq!(mailbox.ready_prefix(usize::MAX), Some(batch.clone()));
    assert_eq!(
        mailbox
            .ready_prefix(PUBLICATION_BODY_LEN + 1 + 100)
            .unwrap()
            .attestations,
        batch.attestations[..1]
    );
    publisher.publish(authority.epoch(), 8, None);
    tokio::time::timeout(crate::endpoint::HANDLE_DEADLINE, async {
        while mailbox.ready_prefix(usize::MAX).is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(client
        .attest_review_publication(&context, repo, &package, &witness)
        .await
        .is_err());
    publisher.publish(authority.epoch(), 7, Some(output));
    client
        .queue_review_publication(repo, &package, &witness, &batch)
        .await
        .unwrap();
    let mut replacement = state.clone();
    replacement.approval.approval.payload = String::from_utf8(
        PatchVotePayloadV1::accept(
            repo.id().into_sha1().unwrap().into(),
            (*patch_id).into_sha1().unwrap().into(),
            later_revision.into_inner().into_sha1().unwrap().into(),
            base.into_sha1().unwrap().into(),
        )
        .encode()
        .unwrap(),
    )
    .unwrap();
    *reader.state.lock().unwrap() = Some(replacement);
    assert!(client
        .sign_review(&context, repo, &prepare())
        .await
        .is_err());
    assert!(client
        .attest_review_publication(&context, repo, &package, &witness)
        .await
        .is_err());
    assert!(client
        .queue_review_publication(repo, &package, &witness, &batch)
        .await
        .is_err());
    use crate::manager::EndpointResolver as _;
    resolver
        .refresh(&crate::manager::FinalizedSnapshot {
            block: authority.block(),
            validators: vec![],
            registry_generation: 1,
            repositories: vec![],
        })
        .await
        .unwrap();
    assert!(mailbox.ready_prefix(usize::MAX).is_none());
    *reader.state.lock().unwrap() = Some(state.clone());
    assert!(client
        .sign_review_refs(&context, repo, &refs, &wrong_signature)
        .await
        .is_err());
    let mut foreign = chain;
    foreign.genesis_hash = B256::repeat_byte(99);
    assert!(LocalReceiptClient::new(&control, foreign)
        .sign_review(&context, repo, &prepare())
        .await
        .is_err());
    *reader.state.lock().unwrap() = None;
    assert!(client
        .sign_review(&context, repo, &prepare())
        .await
        .is_err());
    *reader.state.lock().unwrap() = Some(state);
    let mut expected_refs = context.snapshot.owner_refs().clone();
    expected_refs.insert(
        radicle::storage::refs::SIGREFS_BRANCH.to_ref_string(),
        context.snapshot.sigrefs(),
    );
    assert_eq!(
        repo.references_of(&RepositoryKey::from(
            BlsPublicKey::from_bytes(context.state.checkpoint().receipt().body.owner_bls_public.0)
                .unwrap()
        ))
        .unwrap(),
        expected_refs
    );
    client
        .queue_review_publication(repo, &package, &witness, &batch)
        .await
        .unwrap();
    assert_eq!(mailbox.ready_prefix(usize::MAX), Some(batch));
    stop.send(()).unwrap();
    socket.await.unwrap().unwrap();
    resolver.shutdown().await.unwrap();
    actor.await.unwrap().unwrap();
    assert!(mailbox.ready_prefix(usize::MAX).is_none());

    let archive = ReviewPackageStore::open(store_path).unwrap();
    let accepted = FinalizedPublicationReceipt {
        block: crate::manager::FinalizedBlock {
            number: 19,
            hash: B256::repeat_byte(19),
        },
        receipt: outbe_radicleregistry::publication::PublicationReceipt {
            body: package.body().clone(),
            receipt_hash: package.body().receipt_hash(),
            accepted_height: 18,
        },
    };
    let reopen = || {
        archive.open_checkpoint_repository(
            reader.as_ref(),
            &accepted,
            repo.raw().path(),
            git::UserInfo {
                alias: radicle::node::Alias::new("Reopened"),
                key: *author.public_key(),
            },
        )
    };
    let owner =
        RepositoryKey::from(BlsPublicKey::from_bytes(package.body().owner_bls_public.0).unwrap());
    let before = repo.references_of(&owner).unwrap();
    assert!(
        reopen().is_err(),
        "a local package alone does not confer read grants"
    );
    reader.accepted.lock().unwrap().push(accepted.clone());
    assert!(
        reopen().is_err(),
        "the exact own-chain approval is also required"
    );
    let mut changed = context.state.approval().clone();
    changed.approval.approved_timestamp += 1;
    *reader.approval.lock().unwrap() = Some(changed);
    assert!(reopen().is_err());
    *reader.approval.lock().unwrap() = Some(context.state.approval().clone());
    // Accepted read history remains valid without any current signing permission.
    *reader.state.lock().unwrap() = None;
    let reopened = reopen().unwrap();
    let review = package
        .manifest()
        .refs()
        .iter()
        .find(|r| r.name != radicle::storage::refs::SIGREFS_BRANCH.as_str())
        .unwrap();
    let review: git::Oid = review.new.parse().unwrap();
    assert_eq!(
        cob::change::Storage::load(&reopened, review).unwrap().id,
        review
    );
    let sigrefs: git::Oid = package
        .manifest()
        .refs()
        .iter()
        .find(|r| r.name == radicle::storage::refs::SIGREFS_BRANCH.as_str())
        .unwrap()
        .new
        .parse()
        .unwrap();
    assert!(
        radicle::storage::refs::SignedRefs::load_at(sigrefs, owner, &reopened)
            .unwrap()
            .is_some()
    );
    let mut next_snapshot = context.snapshot.clone();
    next_snapshot
        .advance(accepted.clone(), package.manifest())
        .unwrap();
    let mut next_approval = context.state.approval().clone();
    next_approval.block = accepted.block();
    next_approval.approval.proposal_id = U256::from(6);
    next_approval.approval.approved_height = 19;
    next_approval.approval.approved_timestamp = 1700000019;
    next_approval.approval.payload = String::from_utf8(
        PatchVotePayloadV1::accept(
            repo.id().into_sha1().unwrap().into(),
            (*patch_id).into_sha1().unwrap().into(),
            later_revision.into_inner().into_sha1().unwrap().into(),
            base.into_sha1().unwrap().into(),
        )
        .encode()
        .unwrap(),
    )
    .unwrap();
    let next_signing = context
        .material
        .review_context(accepted.receipt().receipt_hash.0);
    assert!(prepare_patch_review_from_checkpoint(
        repo,
        &next_approval,
        &next_snapshot,
        &next_signing
    )
    .is_err());
    prepare_patch_review_from_checkpoint(&reopened, &next_approval, &next_snapshot, &next_signing)
        .unwrap();
    assert_eq!(repo.references_of(&owner).unwrap(), before);
}
