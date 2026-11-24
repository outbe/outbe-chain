//! Stub implementations for running the marshal in follower mode.
//!
//! In follower mode the node never broadcasts blocks (it has no consensus
//! peers and no signing key admitted to the validators' `authenticated::lookup`
//! network - transport A, the upstream-RPC model). The marshal's `start`
//! nonetheless requires a `buffered::Mailbox` for its broadcast buffer. This
//! module provides a null broadcast: a [`buffered::Engine`] over a random
//! ephemeral key with an empty static peer set, so nothing is ever sent.
//!
//! The stubs are keyed by `bls12381::PublicKey`, Outbe's consensus identity.

use std::num::NonZeroUsize;

use commonware_actor::Feedback;
use commonware_broadcast::buffered;
use commonware_cryptography::bls12381::{self, PrivateKey};
use commonware_cryptography::Signer as _;
use commonware_p2p::{utils::StaticProvider, BlockedSubscription, Blocker};
use commonware_parallel::Sequential;
use commonware_runtime::{Clock, Metrics, Spawner};
use commonware_utils::{channel::ring, ordered::Set, NZUsize};

use crate::block::ConsensusBlock;

/// No-op peer blocker for the follower's null broadcast engine (which has no peers).
#[derive(Clone, Default)]
struct NoopBlocker;

impl Blocker for NoopBlocker {
    type PublicKey = bls12381::PublicKey;

    fn block(&mut self, _peer: Self::PublicKey) -> Feedback {
        Feedback::Ok
    }

    fn blocked(&mut self) -> BlockedSubscription<Self::PublicKey> {
        let (_tx, rx) = ring::channel(NZUsize!(1));
        rx
    }
}

/// Build a null broadcast mailbox for the follower marshal.
///
/// The returned mailbox is the same type the validator path hands to
/// `marshal_actor.start(..)` ([`BroadcastMailbox`](crate::marshal_types::BroadcastMailbox)),
/// but it is backed by an engine that has no peers, so the follower never
/// disseminates anything. The engine handle is dropped intentionally: the
/// follower drives block ingestion entirely through the resolver + upstream,
/// not the broadcast path.
pub(super) fn null_broadcast<E>(
    context: E,
    mailbox_size: NonZeroUsize,
) -> buffered::Mailbox<bls12381::PublicKey, ConsensusBlock>
where
    E: Clock + Spawner + Metrics,
{
    // Deterministic ephemeral key - never used to sign, never admitted anywhere.
    let private_key = PrivateKey::from_seed(0);
    let public_key = private_key.public_key();

    let config = buffered::Config {
        public_key,
        mailbox_size,
        deque_size: 0,
        priority: false,
        codec_config: (),
        peer_provider: StaticProvider::new(0, Set::default()),
        blocker: NoopBlocker,
        strategy: Sequential,
    };

    let (_engine, mailbox) = buffered::Engine::new(context, config);
    mailbox
}
