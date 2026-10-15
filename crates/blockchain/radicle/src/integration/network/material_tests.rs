use super::shutdown_tests::{NoSend, TestReceiver};
use super::*;
use crate::{integration::RadicleStatusChannel, manager::FinalizedRepositoryAuthority};
use alloy_primitives::B256;
use commonware_codec::Encode as _;
use commonware_cryptography::Signer as _;
use commonware_utils::TryCollect as _;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

struct AuthorityReader {
    current: Mutex<Option<FinalizedRepositoryAuthority>>,
    calls: AtomicUsize,
}

impl RepositoryAuthorityReader for AuthorityReader {
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
