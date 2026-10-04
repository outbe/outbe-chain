use super::super::network::{MockReceiver, MockSender};
use super::*;
use commonware_runtime::{Spawner as _, Supervisor as _};

type PlayerHandle = commonware_runtime::Handle<eyre::Result<DkgComplete>>;

pub(super) struct PreviousCommittee {
    pub keys: Vec<bls12381::PrivateKey>,
    pub participants: Set<bls12381::PublicKey>,
    pub output: Output<MinSig, bls12381::PublicKey>,
    pub shares: Vec<Share>,
}

pub(super) fn previous_committee() -> PreviousCommittee {
    let mut keys: Vec<_> = (1..=4).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let (participants, output, shares) = run_direct_initial_round(&keys);
    PreviousCommittee {
        keys,
        participants,
        output,
        shares,
    }
}

pub(super) struct PreviousShares<'a> {
    pub keys: &'a [bls12381::PrivateKey],
    pub output: &'a Output<MinSig, bls12381::PublicKey>,
    pub shares: &'a [Share],
}

pub(super) struct Players {
    pub progress_rx: mpsc::UnboundedReceiver<DkgProgress>,
    pub finalized_log_txs: Vec<mpsc::UnboundedSender<Bytes>>,
    pub handles: Vec<PlayerHandle>,
}

pub(super) struct PlayerCommittee<'a> {
    pub keys: &'a [bls12381::PrivateKey],
    pub participants: &'a Set<bls12381::PublicKey>,
    pub previous: PreviousShares<'a>,
}

struct PlayerInput {
    key: bls12381::PrivateKey,
    participants: Set<bls12381::PublicKey>,
    output: Output<MinSig, bls12381::PublicKey>,
    share: Option<Share>,
    progress_tx: mpsc::UnboundedSender<DkgProgress>,
}

fn spawn_player(
    context: &commonware_runtime::deterministic::Context,
    input: PlayerInput,
    network: (MockSender, MockReceiver),
) -> (mpsc::UnboundedSender<Bytes>, PlayerHandle) {
    let (sender, receiver) = network;
    let (finalized_log_tx, finalized_log_rx) = mpsc::unbounded_channel();
    let handle = context
        .child("dkg_ceremony")
        .spawn(move |clock| async move {
            run_initial_dkg(
                &clock,
                input.key,
                input.participants,
                Some(input.output),
                input.share,
                1,
                Some(input.progress_tx),
                Some(finalized_log_rx),
                sender,
                receiver,
            )
            .await
        });
    (finalized_log_tx, handle)
}

fn player_input(
    committee: &PlayerCommittee<'_>,
    key: bls12381::PrivateKey,
    progress_tx: mpsc::UnboundedSender<DkgProgress>,
) -> PlayerInput {
    let share = committee
        .previous
        .keys
        .iter()
        .position(|old_key| old_key.public_key() == key.public_key())
        .map(|idx| committee.previous.shares[idx].clone());
    PlayerInput {
        key,
        participants: committee.participants.clone(),
        output: committee.previous.output.clone(),
        share,
        progress_tx,
    }
}

pub(super) fn spawn_players(
    context: &commonware_runtime::deterministic::Context,
    committee: PlayerCommittee<'_>,
    network: (Vec<MockSender>, Vec<MockReceiver>),
) -> Players {
    let (senders, receivers) = network;
    let (progress_tx, progress_rx) = mpsc::unbounded_channel();
    let mut finalized_log_txs = Vec::new();
    let mut handles = Vec::new();
    for ((key, sender), receiver) in committee.keys.iter().cloned().zip(senders).zip(receivers) {
        let (tx, handle) = spawn_player(
            context,
            player_input(&committee, key, progress_tx.clone()),
            (sender, receiver),
        );
        finalized_log_txs.push(tx);
        handles.push(handle);
    }
    drop(progress_tx);
    Players {
        progress_rx,
        finalized_log_txs,
        handles,
    }
}

pub(super) struct LogSelection<'a> {
    pub threshold: usize,
    pub required: Option<&'a bls12381::PublicKey>,
}

pub(super) struct LogVerification {
    pub info: Info<MinSig, bls12381::PublicKey>,
    max_players: NonZeroU32,
}

impl LogVerification {
    pub fn new(
        previous: &Set<bls12381::PublicKey>,
        output: &Output<MinSig, bls12381::PublicKey>,
        target: &Set<bls12381::PublicKey>,
    ) -> eyre::Result<Self> {
        let info = Info::new::<N3f1>(
            &crate::config::outbe_app_namespace(),
            1,
            Some(output.clone()),
            Mode::NonZeroCounter,
            commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
            previous.clone(),
            target.clone(),
        )?;
        Ok(Self {
            info,
            max_players: NonZeroU32::new(target.len() as u32)
                .ok_or_else(|| eyre::eyre!("empty target committee"))?,
        })
    }

    pub async fn collect(
        &self,
        progress_rx: &mut mpsc::UnboundedReceiver<DkgProgress>,
        selection: LogSelection<'_>,
        keep: impl Fn(&bls12381::PublicKey) -> bool,
    ) -> eyre::Result<BTreeMap<bls12381::PublicKey, Bytes>> {
        let mut logs = BTreeMap::new();
        while logs.len() < selection.threshold
            || selection
                .required
                .is_some_and(|dealer| !logs.contains_key(dealer))
        {
            let progress = progress_rx
                .recv()
                .await
                .ok_or_else(|| eyre::eyre!("progress channel should remain open"))?;
            let DkgProgress::LocalDealerLog(bytes) = progress else {
                continue;
            };
            let mut reader = bytes.as_ref();
            let signed = SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(
                &mut reader,
                &self.max_players,
            )?;
            let (dealer, _log) = signed
                .check(&self.info)
                .ok_or_else(|| eyre::eyre!("dealer signature must match committee"))?;
            if keep(&dealer) {
                logs.entry(dealer).or_insert(bytes);
            }
        }
        Ok(logs)
    }
}

fn spawn_dealer_only(
    context: &commonware_runtime::deterministic::Context,
    input: PlayerInput,
    network: (MockSender, MockReceiver),
) -> commonware_runtime::Handle<eyre::Result<DkgDealerOnlyComplete>> {
    let (sender, receiver) = network;
    // This role is an old committee member, so its previous share must exist.
    context
        .child("dkg_ceremony")
        .spawn(move |clock| async move {
            let share = input
                .share
                .ok_or_else(|| eyre::eyre!("removed dealer belongs to previous committee"))?;
            run_reshare_dealer_only(
                &clock,
                input.key,
                input.participants,
                input.output,
                share,
                1,
                input.progress_tx,
                sender,
                receiver,
            )
            .await
        })
}

pub(super) fn spawn_with_removed_dealer(
    context: &commonware_runtime::deterministic::Context,
    committee: PlayerCommittee<'_>,
    removed: &bls12381::PublicKey,
    network: (Vec<MockSender>, Vec<MockReceiver>),
) -> (
    Players,
    Option<commonware_runtime::Handle<eyre::Result<DkgDealerOnlyComplete>>>,
) {
    let (senders, receivers) = network;
    let (progress_tx, progress_rx) = mpsc::unbounded_channel();
    let mut finalized_log_txs = Vec::new();
    let mut handles = Vec::new();
    let mut dealer_only_handle = None;
    for ((key, sender), receiver) in committee.keys.iter().cloned().zip(senders).zip(receivers) {
        let is_removed = key.public_key() == *removed;
        let input = player_input(&committee, key, progress_tx.clone());
        if is_removed {
            dealer_only_handle = Some(spawn_dealer_only(context, input, (sender, receiver)));
        } else {
            let (tx, handle) = spawn_player(context, input, (sender, receiver));
            finalized_log_txs.push(tx);
            handles.push(handle);
        }
    }
    drop(progress_tx);
    (
        Players {
            progress_rx,
            finalized_log_txs,
            handles,
        },
        dealer_only_handle,
    )
}
