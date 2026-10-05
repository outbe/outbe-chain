//! Shared delivery of real DKG dealings for test fixtures.

use commonware_cryptography::bls12381::dkg::feldman_desmedt::{
    Dealer, DealerPrivMsg, DealerPubMsg, Player,
};
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_utils::N3f1;

pub struct FixtureDealings<'a> {
    pub public_messages: &'a [DealerPubMsg<MinSig>],
    pub private_messages: &'a [Vec<(bls12381::PublicKey, DealerPrivMsg)>],
}

/// Deliver fixture dealings and return valid player acknowledgements to each dealer.
pub fn acknowledge_fixture_dealings(
    keys: &[bls12381::PrivateKey],
    dealings: FixtureDealings<'_>,
    dealers: &mut [Dealer<MinSig, bls12381::PrivateKey>],
    players: &mut [Player<MinSig, bls12381::PrivateKey>],
) {
    let FixtureDealings {
        public_messages,
        private_messages,
    } = dealings;
    for (dealer_idx, (pub_msg, priv_msgs)) in public_messages
        .iter()
        .zip(private_messages.iter())
        .enumerate()
    {
        let dealer_pk = keys[dealer_idx].public_key();
        for (player_pk, priv_msg) in priv_msgs {
            let player_idx = keys
                .iter()
                .position(|k| &k.public_key() == player_pk)
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
}
