//! Actual automatic workers, native sidecars, protected SDK sockets and registry admission.
//! DKG dealing, canonical finalization and Commonware transport are fixtures, not production E2E.
#[path = "governments/support.rs"]
mod support;

use alloy_primitives::{Address, B256};
use commonware_codec::Encode as _;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_utils::TryCollect as _;
use outbe_consensus::{bls::bootstrap_dkg_for_participants, hybrid::VrfMaterialProvider};
use outbe_primitives::{
    addresses::RADICLE_REGISTRY_ADDRESS,
    radicle_publication::{
        PublicationBody, RepositoryPublicationMailbox, MAX_PUBLICATION_BATCH_LEN,
    },
    storage::hashmap::HashMapStorageProvider,
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
use outbe_radicleregistry::RadicleRegistry;
use outbe_validatorset::{
    contract::ValidatorSet,
    state::{write_committee_snapshot, CommitteeEntry, CommitteeSnapshot},
};
use radicle::{
    crypto::{ssh::Keystore, Seed},
    git,
    identity::RepoId,
    node::{Handle as _, Node, NodeId},
    profile::Home,
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
        let directory = tempfile::tempdir_in("/private/tmp").unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn automatic_bootstrap_reaches_quorum_with_one_offline_member_and_restarts() {
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
        for member in &committee.committee {
            validators
                .test_register_validator_without_pop(member.address, &member.consensus_pubkey)
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
        "../../radicleregistry/tests/fixtures/governments-genesis-storage.json"
    ))
    .unwrap();
    for (slot, value) in genesis["storage"].as_object().unwrap() {
        state.storage.insert(
            (RADICLE_REGISTRY_ADDRESS, slot.parse().unwrap()),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    let provider = FinalizedProvider::default();
    provider.finalize(3, &state);
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
        publisher.install_local_signer(VrfMaterialProvider::new(
            7,
            dkg.polynomial.clone(),
            Some(dkg.shares[index].clone()),
        ));
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
                    .enter(|s| RadicleRegistry::new(s).apply_publication_batch(&batch))
                    .unwrap();
                accounted.extend(batch.attestations.iter().map(|a| a.participant_index));
                provider.finalize(height, &state);
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
