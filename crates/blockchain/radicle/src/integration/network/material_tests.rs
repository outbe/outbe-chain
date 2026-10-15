use super::shutdown_tests::{NoSend, TestReceiver};
use super::*;
use crate::manager::FinalizedPatchSigningState;
use crate::{integration::RadicleStatusChannel, manager::FinalizedRepositoryAuthority};
use alloy_primitives::B256;
use commonware_codec::Encode as _;
use commonware_cryptography::bls12381::{
    dkg::feldman_desmedt::Output, primitives::variant::MinSig,
};
use commonware_cryptography::Signer as _;
use commonware_utils::TryCollect as _;
use outbe_consensus::hybrid::VrfMaterialProvider;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

struct SigningReader {
    current: Mutex<Option<FinalizedPatchSigningState>>,
    reads: AtomicUsize,
}

impl RepositoryAuthorityReader for SigningReader {
    fn read_current_repository_authority(
        &self,
    ) -> Result<FinalizedRepositoryAuthority, ManagerError> {
        self.current
            .lock()
            .as_ref()
            .map(|s| s.authority().clone())
            .ok_or_else(|| ManagerError::Snapshot("fixture state unavailable".into()))
    }
    fn read_current_patch_signing_state(
        &self,
        proposal: alloy_primitives::U256,
    ) -> Result<FinalizedPatchSigningState, ManagerError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.current
            .lock()
            .as_ref()
            .filter(|s| s.approval().approval.proposal_id == proposal)
            .cloned()
            .ok_or_else(|| ManagerError::Snapshot("fixture permission unavailable".into()))
    }
}

pub(crate) async fn review_partials(
    job: Arc<CheckpointPreparedReview>,
    state: &FinalizedPatchSigningState,
    output: &Output<MinSig, bls12381::PublicKey>,
    providers: &[VrfMaterialProvider<MinSig>],
    keys: &[bls12381::PrivateKey],
) -> Vec<(bls12381::PublicKey, signing::RepositoryPartial)> {
    signing_partials(
        RepositoryReviewJob::Cob(job),
        state,
        output,
        providers,
        keys,
    )
    .await
}

pub(crate) async fn refs_partials(
    job: Arc<CheckpointPreparedReviewRefs>,
    state: &FinalizedPatchSigningState,
    output: &Output<MinSig, bls12381::PublicKey>,
    providers: &[VrfMaterialProvider<MinSig>],
    keys: &[bls12381::PrivateKey],
) -> Vec<(bls12381::PublicKey, signing::RepositoryPartial)> {
    signing_partials(
        RepositoryReviewJob::Refs(job),
        state,
        output,
        providers,
        keys,
    )
    .await
}

async fn request(
    resolver: &EndpointNetworkResolver,
    job: &RepositoryReviewJob,
) -> Result<signing::RepositoryPartial, signing::Error> {
    match job {
        RepositoryReviewJob::Cob(job) => resolver.sign_review(job.clone()).await,
        RepositoryReviewJob::Refs(job) => resolver.sign_review_refs(job.clone()).await,
    }
}

async fn signing_partials(
    job: RepositoryReviewJob,
    state: &FinalizedPatchSigningState,
    output: &Output<MinSig, bls12381::PublicKey>,
    providers: &[VrfMaterialProvider<MinSig>],
    keys: &[bls12381::PrivateKey],
) -> Vec<(bls12381::PublicKey, signing::RepositoryPartial)> {
    let mut contributions = vec![];
    for (index, (provider, key)) in providers.iter().zip(keys).enumerate() {
        let reader = Arc::new(SigningReader {
            current: Mutex::new(Some(state.clone())),
            reads: AtomicUsize::new(0),
        });
        let validator = state.authority().committee().committee[index].address;
        let (_, status) = RadicleStatusChannel::enabled(validator, [1; 32]);
        let (service, resolver, _) = EndpointNetwork::build(
            ChainIdentity {
                chain_id: state.authority().chain_id(),
                genesis_hash: state.authority().genesis_hash(),
            },
            status,
        );
        let mut service = service.with_repository_authority_reader(reader.clone());
        let publisher = service.take_repository_material_publisher().unwrap();
        publisher.publish(
            state.authority().epoch(),
            state.authority().committee().vrf_material_version,
            Some(output),
        );
        let (_, local) = LocalEndpointIdentityChannel::create(LocalEndpointIdentity {
            validator,
            node_id: [1; 32],
            addresses: vec![EndpointAddress::dns("local.example", 8776).unwrap()],
        });
        let (_incoming, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(service.run(
            EndpointTransport {
                sender: NoSend,
                receiver: TestReceiver(receiver),
            },
            EndpointSigningIdentity {
                signer: key.clone(),
                local,
            },
        ));
        assert!(matches!(
            request(&resolver, &job).await,
            Err(signing::Error::Unavailable)
        ));
        publisher.install_local_signer(provider.clone());
        let partial = request(&resolver, &job).await.unwrap();
        let mut consumed = state.clone();
        consumed.stage = outbe_radicleregistry::schema::PATCH_EXECUTION_REVIEWED;
        *reader.current.lock() = Some(consumed);
        assert!(matches!(
            request(&resolver, &job).await,
            Err(signing::Error::Context)
        ));
        let mut advanced = state.clone();
        advanced.checkpoint.receipt.body.sequence += 1;
        advanced.checkpoint.receipt.body.previous_receipt_hash =
            state.checkpoint().receipt().receipt_hash;
        advanced.checkpoint.receipt.receipt_hash = advanced.checkpoint.receipt.body.receipt_hash();
        *reader.current.lock() = Some(advanced);
        assert!(matches!(
            request(&resolver, &job).await,
            Err(signing::Error::Context)
        ));
        let mut substituted = state.clone();
        substituted.approval.approval.approved_timestamp += 1;
        *reader.current.lock() = Some(substituted);
        assert!(matches!(
            request(&resolver, &job).await,
            Err(signing::Error::Context)
        ));
        *reader.current.lock() = None;
        assert!(matches!(
            request(&resolver, &job).await,
            Err(signing::Error::Authority(_))
        ));
        *reader.current.lock() = Some(state.clone());
        publisher.publish(
            state.authority().epoch() + 1,
            state.authority().committee().vrf_material_version + 1,
            Some(output),
        );
        assert!(matches!(
            request(&resolver, &job).await,
            Err(signing::Error::Material)
        ));
        publisher.publish(
            state.authority().epoch(),
            state.authority().committee().vrf_material_version,
            None,
        );
        assert!(matches!(
            request(&resolver, &job).await,
            Err(signing::Error::Unavailable)
        ));
        drop(publisher);
        assert!(matches!(
            request(&resolver, &job).await,
            Err(signing::Error::Unavailable)
        ));
        assert_eq!(reader.reads.load(Ordering::SeqCst), 9);
        resolver.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
        contributions.push((key.public_key(), partial));
    }
    contributions
}

struct AuthorityReader {
    current: Mutex<Option<FinalizedRepositoryAuthority>>,
    calls: AtomicUsize,
}

impl RepositoryAuthorityReader for AuthorityReader {
    fn read_current_patch_signing_state(
        &self,
        _: alloy_primitives::U256,
    ) -> Result<crate::manager::FinalizedPatchSigningState, ManagerError> {
        Err(ManagerError::Snapshot("material-only fixture".into()))
    }
    fn read_current_repository_authority(
        &self,
    ) -> Result<FinalizedRepositoryAuthority, ManagerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.current
            .lock()
            .clone()
            .ok_or_else(|| ManagerError::Snapshot("unavailable test state".into()))
    }
}

#[tokio::test]
async fn repository_material_bridge_requires_matching_live_output_and_fresh_finalized_authority() {
    let mut keys: Vec<_> = (0..4).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key());
    let players = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let dkg = outbe_consensus::bls::bootstrap_dkg_for_participants(players).unwrap();
    let owner = radicle::crypto::BlsPublicKey::from_bytes(
        dkg.polynomial
            .public()
            .encode()
            .as_ref()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    let authority = FinalizedRepositoryAuthority {
        block: crate::manager::FinalizedBlock {
            number: 10,
            hash: B256::repeat_byte(10),
        },
        chain_id: 2026,
        genesis_hash: B256::repeat_byte(7),
        epoch: 1,
        committee: outbe_validatorset::state::CommitteeSnapshot {
            committee: keys
                .iter()
                .enumerate()
                .map(|(i, key)| outbe_validatorset::state::CommitteeEntry {
                    address: Address::repeat_byte(i as u8 + 1),
                    consensus_pubkey: key.public_key().encode().as_ref().try_into().unwrap(),
                })
                .collect(),
            vrf_material_version: 7,
            vrf_group_public_key_bytes: dkg.polynomial.public().encode().to_vec(),
            vrf_public_polynomial_hash: outbe_consensus::dkg_manager::public_polynomial_hash(
                &dkg.polynomial,
            ),
        },
    };
    let reader = Arc::new(AuthorityReader {
        current: Mutex::new(Some(authority.clone())),
        calls: AtomicUsize::new(0),
    });
    let (_, status) = RadicleStatusChannel::enabled(Address::repeat_byte(1), [1; 32]);
    let (service, resolver, _) = EndpointNetwork::build(
        ChainIdentity {
            chain_id: 2026,
            genesis_hash: authority.genesis_hash(),
        },
        status,
    );
    let mut service = service.with_repository_authority_reader(reader.clone());
    let publisher = service.take_repository_material_publisher().unwrap();
    assert!(service.take_repository_material_publisher().is_none());
    let (_, local) = LocalEndpointIdentityChannel::create(LocalEndpointIdentity {
        validator: Address::repeat_byte(1),
        node_id: [1; 32],
        addresses: vec![EndpointAddress::dns("local.example", 8776).unwrap()],
    });
    let (_incoming, receiver) = mpsc::unbounded_channel();
    let task = tokio::spawn(service.run(
        EndpointTransport {
            sender: NoSend,
            receiver: TestReceiver(receiver),
        },
        EndpointSigningIdentity {
            signer: keys[0].clone(),
            local,
        },
    ));
    assert!(matches!(
        resolver.repository_signing_material(owner).await,
        Err(signing::Error::Unavailable)
    ));
    publisher.publish(1, 7, Some(&dkg.output));
    let material = resolver.repository_signing_material(owner).await.unwrap();
    assert_eq!(material.canonical_output(), dkg.output.encode().as_ref());
    let expected =
        signing::RepositorySigningMaterial::new(authority.clone(), dkg.output.clone(), owner)
            .unwrap();
    assert_eq!(material.material_hash(), expected.material_hash());
    let mut foreign = authority.clone();
    foreign.chain_id += 1;
    *reader.current.lock() = Some(foreign);
    assert!(matches!(
        resolver.repository_signing_material(owner).await,
        Err(signing::Error::Material)
    ));
    let mut foreign = authority.clone();
    foreign.genesis_hash = B256::repeat_byte(8);
    *reader.current.lock() = Some(foreign);
    assert!(matches!(
        resolver.repository_signing_material(owner).await,
        Err(signing::Error::Material)
    ));
    *reader.current.lock() = None;
    assert!(matches!(
        resolver.repository_signing_material(owner).await,
        Err(signing::Error::Authority(_))
    ));
    *reader.current.lock() = Some(authority.clone());
    publisher.publish(2, 8, Some(&dkg.output));
    assert!(matches!(
        resolver.repository_signing_material(owner).await,
        Err(signing::Error::Material)
    ));
    let mut next = authority.clone();
    next.epoch = 2;
    next.committee.vrf_material_version = 8;
    *reader.current.lock() = Some(next.clone());
    let ready = resolver.repository_signing_material(owner).await.unwrap();
    assert_eq!(ready.review_context([0; 32]).epoch, 2);
    assert_eq!(ready.review_context([0; 32]).material_version, 8);
    assert_ne!(ready.material_hash(), material.material_hash());
    next.committee.committee.reverse();
    *reader.current.lock() = Some(next.clone());
    assert!(matches!(
        resolver.repository_signing_material(owner).await,
        Err(signing::Error::Material)
    ));
    next.committee.committee.reverse();
    next.committee.vrf_public_polynomial_hash = B256::ZERO;
    *reader.current.lock() = Some(next);
    assert!(matches!(
        resolver.repository_signing_material(owner).await,
        Err(signing::Error::Material)
    ));
    *reader.current.lock() = Some(authority);
    publisher.publish(1, 7, None);
    assert!(matches!(
        resolver.repository_signing_material(owner).await,
        Err(signing::Error::Unavailable)
    ));
    publisher.publish(1, 7, Some(&dkg.output));
    assert!(resolver.repository_signing_material(owner).await.is_ok());
    drop(publisher);
    assert!(matches!(
        resolver.repository_signing_material(owner).await,
        Err(signing::Error::Unavailable)
    ));
    assert_eq!(reader.calls.load(Ordering::SeqCst), 12);
    resolver.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    assert!(matches!(
        resolver.repository_signing_material(owner).await,
        Err(signing::Error::Unavailable)
    ));
}

#[tokio::test]
async fn repository_material_requests_share_the_bounded_nonblocking_local_mailbox() {
    let players = [bls12381::PrivateKey::from_seed(1).public_key()]
        .into_iter()
        .try_collect()
        .unwrap();
    let dkg = outbe_consensus::bls::bootstrap_dkg_for_participants(players).unwrap();
    let owner = radicle::crypto::BlsPublicKey::from_bytes(
        dkg.polynomial
            .public()
            .encode()
            .as_ref()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    let (_, status) = RadicleStatusChannel::enabled(Address::ZERO, [0; 32]);
    let (_service, resolver, _) = EndpointNetwork::build(
        ChainIdentity {
            chain_id: 1,
            genesis_hash: B256::ZERO,
        },
        status,
    );
    for _ in 0..256 {
        let (result, _response) = oneshot::channel();
        resolver
            .commands
            .try_send(NetworkCommand::RepositoryMaterial { owner, result })
            .unwrap();
    }
    assert!(matches!(
        resolver.repository_signing_material(owner).await,
        Err(signing::Error::Unavailable)
    ));
}
