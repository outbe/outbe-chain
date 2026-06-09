//! Type aliases for the Commonware marshal actor integration.
//!
//! Follows the same pattern as Tempo's `alias.rs`, parameterized
//! for outbe-chain's block type and scheme provider.
//!
//! The certificate scheme is `HybridScheme<MinSig>` — matching the Simplex
//! engine. This allows marshal to be wired as a Simplex reporter and receive
//! finalization events directly, enabling the tempo-style recovery path.

use commonware_consensus::marshal;
use commonware_consensus::simplex::types::Finalization as SimplexFinalization;
use commonware_consensus::types::FixedEpocher;
use commonware_cryptography::bls12381::{self, primitives::variant::MinSig};
use commonware_parallel::Sequential;
use commonware_utils::acknowledgement::Exact;

use crate::block::ConsensusBlock;
use crate::digest::Digest;
use crate::hybrid::{HybridScheme, HybridSchemeProvider};

/// Marshal variant: Standard (whole-block, not erasure-coded).
pub type Variant = marshal::standard::Standard<ConsensusBlock>;

/// Certificate scheme used by the marshal — aligned with Simplex's HybridScheme
/// so marshal can be wired as a Simplex reporter for finalization delivery.
pub type CertScheme = HybridScheme<MinSig>;

/// Marshal mailbox handle for interacting with the actor.
pub type MarshalMailbox = marshal::core::Mailbox<CertScheme, Variant>;

/// Finalization type stored in the certificates archive.
pub type Finalization = SimplexFinalization<CertScheme, Digest>;

/// Marshal actor with immutable archive storage.
pub type MarshalActor<E> = marshal::core::Actor<
    E,
    Variant,
    HybridSchemeProvider<MinSig>,
    commonware_storage::archive::immutable::Archive<E, Digest, Finalization>,
    commonware_storage::archive::immutable::Archive<E, Digest, ConsensusBlock>,
    FixedEpocher,
    Sequential,
    Exact,
>;

/// Broadcast engine for block dissemination.
pub type BroadcastEngine<E, D> =
    commonware_broadcast::buffered::Engine<E, bls12381::PublicKey, ConsensusBlock, D>;

/// Broadcast mailbox for block dissemination.
pub type BroadcastMailbox =
    commonware_broadcast::buffered::Mailbox<bls12381::PublicKey, ConsensusBlock>;

/// Update type that marshal sends to the executor reporter.
pub type MarshalUpdate =
    commonware_consensus::marshal::Update<ConsensusBlock, commonware_utils::acknowledgement::Exact>;
