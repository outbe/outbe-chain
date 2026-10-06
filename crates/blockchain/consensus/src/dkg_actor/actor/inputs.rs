use super::DkgProgress;
use alloy_primitives::Bytes;
use commonware_cryptography::bls12381::{
    self,
    dkg::feldman_desmedt::Output,
    primitives::{group::Share, variant::MinSig},
};
use commonware_utils::ordered::Set;
use tokio::sync::mpsc;

/// Identity and previous material for bootstrap or a participant reshare.
pub struct DkgParticipantParameters {
    pub signing_key: bls12381::PrivateKey,
    pub participants: Set<bls12381::PublicKey>,
    pub previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
    pub previous_share: Option<Share>,
    pub round: u64,
}

/// A removed dealer must retain both its previous output and its share.
pub struct DkgDealerParameters {
    pub signing_key: bls12381::PrivateKey,
    pub participants: Set<bls12381::PublicKey>,
    pub previous_output: Output<MinSig, bls12381::PublicKey>,
    pub previous_share: Share,
    pub round: u64,
}

/// Progress and finalized-chain inputs for a participant ceremony.
pub struct DkgProgressChannels {
    pub progress_tx: Option<mpsc::UnboundedSender<DkgProgress>>,
    pub finalized_log_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
}

/// P2P handles owned by one DKG attempt.
pub struct DkgTransport<S, R> {
    pub sender: S,
    pub receiver: R,
}
