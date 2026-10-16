use super::*;
use crate::{
    endpoint::{ChainIdentity, EndpointAddress},
    integration::{
        local::{LocalReceiptClient, LocalReceiptServer},
        network::shutdown_tests::{NoSend, TestReceiver},
        publication::store::ReviewPackageStore,
        review::prepare_patch_review_from_checkpoint,
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
}
impl RepositoryReceiptReader for Reader {
    fn read_current_publication_receipt(
        &self,
        sequence: u64,
    ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError> {
        Ok((sequence == 1).then(|| self.root.clone()))
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
    store: ReviewPackageStore,
    mut root: FinalizedPublicationReceipt,
    material: &RepositorySigningMaterial,
    keys: &[bls12381::PrivateKey],
    providers: &[VrfMaterialProvider<MinSig>],
    output: &Output<MinSig, bls12381::PublicKey>,
) {
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
    });
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
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
    let mut service = service.with_repository_authority_reader(reader.clone());
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
    stop.send(()).unwrap();
    socket.await.unwrap().unwrap();
    resolver.shutdown().await.unwrap();
    actor.await.unwrap().unwrap();
}
