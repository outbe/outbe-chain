//! Type aliases for the Commonware marshal actor integration.
//!
//! These aliases parameterize Commonware marshal for outbe-chain's block type
//! and scheme provider.
//!
//! The certificate scheme is `HybridScheme<MinSig>` - matching the Simplex
//! engine. This allows marshal to be wired as a Simplex reporter and receive
//! finalization events directly, enabling finalization-driven recovery.

use commonware_consensus::marshal;
use commonware_consensus::simplex::types::Finalization as SimplexFinalization;
use commonware_cryptography::bls12381::{self, primitives::variant::MinSig};
use commonware_parallel::Sequential;
use commonware_utils::acknowledgement::Exact;

use crate::block::ConsensusBlock;
use crate::digest::Digest;
use crate::hybrid::{HybridScheme, HybridSchemeProvider};

/// Marshal variant: Standard (whole-block, not erasure-coded).
pub type Variant = marshal::standard::Standard<ConsensusBlock>;

/// Certificate scheme used by the marshal - aligned with Simplex's HybridScheme
/// so marshal can be wired as a Simplex reporter for finalization delivery.
pub type CertScheme = HybridScheme<MinSig>;

/// Marshal mailbox handle for interacting with the actor.
pub type MarshalMailbox = marshal::core::Mailbox<CertScheme, Variant>;

/// Finalization type stored in the certificates archive.
pub type Finalization = SimplexFinalization<CertScheme, Digest>;

/// Marshal actor with immutable archive storage, keyed by
/// [`FollowerEpocher`](crate::follow::FollowerEpocher). Its epoch boundaries
/// match outbe's on-chain committee-epoch boundaries. The epocher takes these
/// bounds from observed activation carriers, not from a fixed formula. Marshal
/// asserts `finalization.epoch() == epocher.containing(height)` on every
/// resolver delivery. Only this epocher makes that hold at boundary blocks and
/// after a grace-delayed activation (see `follow::epocher`). The validator and
/// the follower use the same actor type.
pub type MarshalActor<E> = marshal::core::Actor<
    E,
    Variant,
    HybridSchemeProvider<MinSig>,
    commonware_storage::archive::immutable::Archive<E, Digest, Finalization>,
    commonware_storage::archive::immutable::Archive<E, Digest, ConsensusBlock>,
    crate::follow::FollowerEpocher,
    Sequential,
    Exact,
>;

/// Follower marshal actor. It is the same type as [`MarshalActor`].
pub type FollowMarshalActor<E> = MarshalActor<E>;

/// Broadcast engine for block dissemination.
pub type BroadcastEngine<E, D> =
    commonware_broadcast::buffered::Engine<E, bls12381::PublicKey, ConsensusBlock, D>;

/// Broadcast mailbox for block dissemination.
pub type BroadcastMailbox =
    commonware_broadcast::buffered::Mailbox<bls12381::PublicKey, ConsensusBlock>;

/// Update type that marshal sends to the executor reporter.
pub type MarshalUpdate =
    commonware_consensus::marshal::Update<ConsensusBlock, commonware_utils::acknowledgement::Exact>;
