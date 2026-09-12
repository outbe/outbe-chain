use super::*;

// -----------------------------------------------------------------------
// Mock P2P network - routes messages between in-process nodes
// -----------------------------------------------------------------------

/// Shared routing table: maps public key -> incoming message channel.
type Router =
    Arc<HashMap<bls12381::PublicKey, mpsc::UnboundedSender<(bls12381::PublicKey, IoBuf)>>>;

/// Mock P2P sender that routes messages through in-memory channels.
#[derive(Clone)]
pub(super) struct MockSender {
    my_pk: bls12381::PublicKey,
    router: Router,
    drop_rule: Option<DropRule>,
}

type DropRule = Arc<Mutex<DropMessages>>;

#[derive(Debug)]
pub(super) struct DropMessages {
    pub(super) from: Option<bls12381::PublicKey>,
    pub(super) to: bls12381::PublicKey,
    pub(super) tag: u8,
    pub(super) remaining: usize,
    pub(super) dropped: usize,
}

/// Checked sender returned by MockSender::check().
pub(super) struct MockCheckedSender<'a> {
    sender: &'a MockSender,
    recipients: Recipients<bls12381::PublicKey>,
}

impl commonware_p2p::CheckedSender for MockCheckedSender<'_> {
    type PublicKey = bls12381::PublicKey;

    fn recipients(&self) -> Vec<Self::PublicKey> {
        match &self.recipients {
            Recipients::One(pk) => vec![pk.clone()],
            Recipients::Some(pks) => pks.clone(),
            Recipients::All => self
                .sender
                .router
                .keys()
                .filter(|pk| **pk != self.sender.my_pk)
                .cloned()
                .collect(),
        }
    }

    fn send(
        self,
        message: impl Into<commonware_runtime::IoBufs> + Send,
        _priority: bool,
    ) -> commonware_actor::Unreliable<commonware_actor::Feedback> {
        let data: IoBuf = message.into().coalesce();
        for target in self.recipients() {
            if should_drop_message_once(
                &self.sender.drop_rule,
                &self.sender.my_pk,
                &target,
                data.as_ref(),
            ) {
                continue;
            }
            if let Some(tx) = self.sender.router.get(&target) {
                let _ = tx.send((self.sender.my_pk.clone(), data.clone()));
            }
        }
        commonware_actor::Unreliable::Outcome(commonware_actor::Feedback::Ok)
    }
}

impl commonware_p2p::LimitedSender for MockSender {
    type PublicKey = bls12381::PublicKey;
    type Checked<'a> = MockCheckedSender<'a>;

    fn check(
        &mut self,
        recipients: Recipients<Self::PublicKey>,
    ) -> Result<Self::Checked<'_>, std::time::SystemTime> {
        Ok(MockCheckedSender {
            sender: self,
            recipients,
        })
    }
}

/// Mock P2P receiver backed by an mpsc channel.
#[derive(Debug)]
pub(super) struct MockReceiver {
    rx: mpsc::UnboundedReceiver<(bls12381::PublicKey, IoBuf)>,
}

impl commonware_p2p::Receiver for MockReceiver {
    type Error = std::io::Error;
    type PublicKey = bls12381::PublicKey;

    async fn recv(&mut self) -> Result<commonware_p2p::Message<Self::PublicKey>, Self::Error> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }
}

/// Build a mock P2P mesh for `n` nodes.
///
/// Returns `(keys, senders, receivers)` where each index corresponds to a node.
pub(super) fn build_mock_network(
    keys: &[bls12381::PrivateKey],
) -> (Vec<MockSender>, Vec<MockReceiver>) {
    build_mock_network_with_drop(keys, None)
}

pub(super) fn build_mock_network_with_drop(
    keys: &[bls12381::PrivateKey],
    drop_rule: Option<DropRule>,
) -> (Vec<MockSender>, Vec<MockReceiver>) {
    let mut channels = HashMap::new();
    let mut receivers = Vec::new();

    for key in keys {
        let pk = key.public_key();
        let (tx, rx) = mpsc::unbounded_channel();
        channels.insert(pk, tx);
        receivers.push(MockReceiver { rx });
    }

    let router: Router = Arc::new(channels);

    let senders: Vec<MockSender> = keys
        .iter()
        .map(|k| MockSender {
            my_pk: k.public_key(),
            router: Arc::clone(&router),
            drop_rule: drop_rule.clone(),
        })
        .collect();

    (senders, receivers)
}

#[allow(clippy::type_complexity)]
pub(super) fn run_direct_initial_round(
    keys: &[bls12381::PrivateKey],
) -> (
    Set<bls12381::PublicKey>,
    Output<MinSig, bls12381::PublicKey>,
    Vec<Share>,
) {
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &crate::config::outbe_app_namespace(),
        0,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants.clone(),
    )
    .unwrap();

    let mut dealers = Vec::new();
    let mut pub_msgs = Vec::new();
    let mut all_priv_msgs = Vec::new();
    for key in keys {
        let (dealer, pub_msg, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
            info.clone(),
            key.clone(),
            None,
        )
        .unwrap();
        dealers.push(dealer);
        pub_msgs.push(pub_msg);
        all_priv_msgs.push(priv_msgs);
    }

    let mut players: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
        .iter()
        .map(|key| Player::new(info.clone(), key.clone()).unwrap())
        .collect();

    for (dealer_idx, (pub_msg, priv_msgs)) in pub_msgs.iter().zip(all_priv_msgs.iter()).enumerate()
    {
        let dealer_pk = keys[dealer_idx].public_key();
        for (player_pk, priv_msg) in priv_msgs {
            let player_idx = keys
                .iter()
                .position(|key| key.public_key() == *player_pk)
                .unwrap();
            if let Some(ack) = players[player_idx]
                .dealer_message::<N3f1>(dealer_pk.clone(), pub_msg.clone(), priv_msg.clone())
                .expect("fixture dealing must be valid")
            {
                dealers[dealer_idx]
                    .receive_player_ack(player_pk.clone(), ack)
                    .unwrap();
            }
        }
    }

    let mut logs = BTreeMap::new();
    for dealer in dealers {
        let signed_log = dealer.finalize::<N3f1>();
        if let Some((dealer_pk, log)) = signed_log.check(&info) {
            logs.insert(dealer_pk, log);
        }
    }

    let mut output = None;
    let mut shares = Vec::new();
    for player in players {
        let mut dkg_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info.clone());
        for (dealer_pk, log) in &logs {
            dkg_logs.record(dealer_pk.clone(), log.clone());
        }
        let (player_output, share) = player
            .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
                &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                dkg_logs,
                &Sequential,
            )
            .unwrap();
        output = Some(player_output);
        shares.push(share);
    }

    (participants, output.unwrap(), shares)
}

fn should_drop_message_once(
    drop_rule: &Option<DropRule>,
    from: &bls12381::PublicKey,
    to: &bls12381::PublicKey,
    payload: &[u8],
) -> bool {
    let Some(drop_rule) = drop_rule else {
        return false;
    };
    let Some(tag) = payload.get(1 + std::mem::size_of::<u64>() + B256::len_bytes()) else {
        return false;
    };
    let Ok(mut rule) = drop_rule.lock() else {
        return false;
    };
    if rule.remaining == 0
        || rule.tag != *tag
        || rule.from.as_ref().is_some_and(|expected| expected != from)
        || &rule.to != to
    {
        return false;
    }
    rule.remaining -= 1;
    rule.dropped += 1;
    true
}

// -----------------------------------------------------------------------
// Offline-node tests - initial interactive bootstrap must not finalize from
// arbitrary threshold P2P subsets because no chain carrier exists yet.
// -----------------------------------------------------------------------

/// Helper: run DKG with `n` total keys but only spawn tasks for the first
/// `online` nodes. Offline nodes' receivers are dropped so they never
/// participate. Returns results from online nodes only.
pub(super) async fn run_partial_dkg(
    clock: &commonware_runtime::deterministic::Context,
    n: usize,
    online: usize,
) -> Vec<Result<DkgComplete>> {
    use commonware_runtime::{Spawner as _, Supervisor as _};
    assert!(online <= n);

    let mut keys: Vec<bls12381::PrivateKey> = (0..n)
        .map(|_| {
            bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
        })
        .collect();
    keys.sort_by_key(|a| a.public_key().encode());

    // Participant set includes ALL n validators (this is the "expected" set).
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|k| k.public_key())
        .try_collect::<Set<bls12381::PublicKey>>()
        .unwrap();

    let (senders, receivers) = build_mock_network(&keys);

    // Only spawn DKG tasks for the first `online` nodes.
    // The remaining nodes' receivers are dropped (simulating offline).
    let mut handles = Vec::new();
    for (key, sender, receiver) in keys
        .iter()
        .take(online)
        .cloned()
        .zip(senders.into_iter().take(online))
        .zip(receivers.into_iter().take(online))
        .map(|((k, s), r)| (k, s, r))
    {
        let p = participants.clone();
        handles.push(clock.child("dkg_ceremony").spawn(move |clock| async move {
            run_initial_dkg(&clock, key, p, None, None, 0, None, None, sender, receiver).await
        }));
    }
    // Drop remaining receivers explicitly (offline nodes).
    // (They're already dropped by the `take(online)` iterators above,
    // but this documents intent.)

    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    results
}
