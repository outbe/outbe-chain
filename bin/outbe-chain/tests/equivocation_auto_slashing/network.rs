use commonware_codec::{DecodeExt, Encode};
use commonware_consensus::{
    simplex::types::{Finalize, Notarize, Nullify, Proposal, Vote},
    types::{Epoch, Round, View},
    Viewable as _,
};
use commonware_cryptography::{
    bls12381::{self, primitives::variant::MinSig},
    Signer as _,
};
use commonware_p2p::{
    authenticated::lookup, utils::mux::Muxer, AddressableManager as _, Receiver as _, Recipients,
    Sender as _,
};
use commonware_runtime::{Quota, Runner as _, Supervisor as _};
use commonware_utils::TryCollect as _;
use outbe_consensus::{
    bls::{self, KeyBackend},
    config,
    digest::Digest,
    hybrid::HybridScheme,
};
use std::{
    net::SocketAddr,
    num::{NonZeroU32, NonZeroUsize},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
};

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    ConflictingNotarize,
    ConflictingFinalize,
    NullifyFinalize,
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ConflictingNotarize => "conflicting_notarize",
            Self::ConflictingFinalize => "conflicting_finalize",
            Self::NullifyFinalize => "nullify_finalize",
        }
    }
    fn votes(self, scheme: &HybridScheme<MinSig>, view: View) -> [Vec<u8>; 2] {
        let round = Round::new(Epoch::new(0), view);
        let a = Proposal::new(
            round,
            View::new(0),
            Digest::from(alloy_primitives::B256::repeat_byte(0xa1)),
        );
        let b = Proposal::new(
            round,
            View::new(0),
            Digest::from(alloy_primitives::B256::repeat_byte(0xb2)),
        );
        let votes = match self {
            Self::ConflictingNotarize => [
                Vote::Notarize(Notarize::sign(scheme, a).unwrap()),
                Vote::Notarize(Notarize::sign(scheme, b).unwrap()),
            ],
            Self::ConflictingFinalize => [
                Vote::Finalize(Finalize::sign(scheme, a).unwrap()),
                Vote::Finalize(Finalize::sign(scheme, b).unwrap()),
            ],
            Self::NullifyFinalize => [
                Vote::Nullify(Nullify::sign::<Digest>(scheme, round).unwrap()),
                Vote::Finalize(Finalize::sign(scheme, a).unwrap()),
            ],
        };
        votes.map(|vote| vote.encode().to_vec())
    }
}
pub struct Injector {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Injector {
    pub fn start(root: &Path, peers: Vec<(bls12381::PublicKey, SocketAddr)>, kind: Kind) -> Self {
        config::init_consensus_chain_id(424242).unwrap();
        let key = bls::load_individual_key(
            &root.join("validator-3/signing-key.hex"),
            &KeyBackend::Plaintext,
        )
        .unwrap();
        let keys = root.join("validator-3/dkg-keys");
        let polynomial =
            bls::load_public_polynomial(&keys.join("dkg_polynomial.hex"), &KeyBackend::Plaintext)
                .unwrap();
        let share =
            bls::load_signing_share(&keys.join("dkg_share.hex"), &KeyBackend::Plaintext).unwrap();
        let participants = peers
            .iter()
            .map(|(key, _)| key.clone())
            .try_collect()
            .unwrap();
        let scheme = HybridScheme::<MinSig>::signer(
            &config::outbe_app_namespace(),
            participants,
            key.clone(),
            polynomial,
            share,
        )
        .unwrap();
        let listen = peers
            .iter()
            .find(|(peer, _)| *peer == key.public_key())
            .unwrap()
            .1;
        let directory = root.join("injector");
        let stop = Arc::new(AtomicBool::new(false));
        let running = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let runner = commonware_runtime::tokio::Runner::new(
                commonware_runtime::tokio::Config::new().with_storage_directory(directory),
            );
            runner.start(|ctx| async move {
                let namespace =
                    commonware_utils::union_unique(&config::outbe_app_namespace(), b"_P2P");
                let cfg = lookup::Config::local(
                    key,
                    &namespace,
                    listen,
                    NonZeroUsize::new(5).unwrap(),
                    config::MAX_P2P_MESSAGE_SIZE,
                );
                let (mut network, mut oracle) = lookup::Network::new(ctx.child("network"), cfg);
                let (tx, rx) =
                    network.register(0, Quota::per_second(NonZeroU32::new(128).unwrap()));
                let map = peers
                    .into_iter()
                    .map(|(key, address)| (key, commonware_p2p::Address::from(address)))
                    .try_collect()
                    .unwrap();
                oracle.track(
                    0,
                    commonware_p2p::AddressableTrackedPeers::new(
                        map,
                        commonware_utils::ordered::Map::default(),
                    ),
                );
                let _network = network.start();
                let (muxer, mut mux) = Muxer::new(ctx.child("votes"), tx, rx, 1024);
                let _muxer = muxer.start();
                let (mut sender, mut receiver) = mux.register(0).await.unwrap();
                let mut injected_view = None;
                while !running.load(Ordering::Relaxed) {
                    let Ok(Ok((_, bytes))) = tokio::time::timeout(
                        std::time::Duration::from_millis(200),
                        receiver.recv(),
                    )
                    .await
                    else {
                        continue;
                    };
                    let Ok(vote) = Vote::<HybridScheme<MinSig>, Digest>::decode(bytes) else {
                        continue;
                    };
                    if vote.view().get() < 2 {
                        continue;
                    }
                    let view = *injected_view.get_or_insert(vote.view());
                    for bytes in kind.votes(&scheme, view) {
                        let _ = sender.send(Recipients::All, bytes, true);
                    }
                }
            });
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Injector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
