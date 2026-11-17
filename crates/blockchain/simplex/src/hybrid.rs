//! Hybrid signing scheme: BLS12-381 individual attribution + decoupled threshold VRF.
//!
//! Combines BLS individual signatures (MinPk - attributable, aggregatable) with
//! BLS12-381 threshold signatures (MinSig - VRF for unpredictable leader election).
//!
//! Each validator produces a BLS individual vote signature and a BLS threshold
//! seed partial. Without VRF material, a validator does not sign. The consensus
//! certificate aggregates individual vote signatures into a single 96-byte
//! aggregate signature. The certificate also carries the recovered threshold
//! VRF proof. This proof is mandatory: certificate assembly fails without a
//! VRF quorum, and certificate verification rejects a missing or invalid proof.
//!
//! Properties:
//! - `is_attributable() = true` - signer bitmap provides per-validator evidence
//! - `is_batchable() = false` - current Outbe path verifies attestations sequentially
//! - VRF seed extractable from the verified certificate's VRF proof for randomness
//! - Certificate size: ~162 bytes for any number of validators (vs ~9,300 for ed25519 variant)
//!
//! ## Signature Verification Timing
//!
//! Commonware's batcher stores votes BEFORE cryptographic verification for batch
//! efficiency. The batcher queues votes and batch-verifies them periodically via
//! `verify_notarizes()`. Invalid signers are blocked after batch verification completes.
//! This creates a bounded timing window where a malicious vote temporarily occupies a
//! slot in `pending_votes`. Fast batch cycles and immediate peer blocking on
//! verification failure mitigate this window. This is Commonware's intentional design
//! choice to optimize throughput.

use bytes::{Buf, BufMut};
use commonware_codec::{Encode, Error, FixedSize, Read, ReadExt, Write};
use commonware_consensus::{
    simplex::{scheme::Namespace, types::Subject},
    types::{Epoch, Round},
};
use commonware_cryptography::{
    bls12381::{
        self,
        primitives::{
            group::Share,
            ops::aggregate,
            sharing::Sharing,
            variant::{MinPk, PartialSignature, Variant},
        },
    },
    certificate::{self, Attestation, Signers, Subject as CertificateSubject, Verification},
    Digest, Signer as _, Verifier as _,
};
use commonware_parallel::Strategy;
use commonware_utils::{
    iter::NonEmpty,
    ordered::{Quorum, Set},
    N3f1, Participant,
};
use rand_commonware::rngs::SysRng;
use rand_core_commonware::{CryptoRng, UnwrapErr};
use std::sync::Arc;

/// VRF-based leader election for the hybrid scheme (lifted out of this file).
pub mod election;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;
mod vrf_material;

use vrf_material::VrfPartialVerification;
pub use vrf_material::{RepositorySigningError, VrfMaterialProvider};

/// CSPRNG allowed only for Commonware BLS batch-verification weights.
///
/// Commonware's batch verifier samples unpredictable scalar weights to prevent
/// invalid signatures from cancelling out in aggregate. This RNG must not feed
/// VRF seed derivation, leader election, header fields, metadata encoding, or
/// any consensus state transition.
pub fn bls_batch_verification_rng() -> UnwrapErr<SysRng> {
    UnwrapErr(SysRng)
}

/// Combined BLS individual vote + BLS threshold seed partial emitted by each validator.
///
/// Contains two components:
/// - BLS MinPk individual signature over the vote message (attributable, aggregatable)
/// - BLS MinSig threshold partial over the seed message (for VRF seed recovery)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HybridSignature<V: Variant> {
    /// BLS individual vote signature (MinPk, 96 bytes).
    pub bls_individual_vote: bls12381::Signature,
    /// Version of the VRF material used for `bls_seed_partial`.
    pub vrf_material_version: u64,
    /// BLS threshold partial over the seed message (MinSig, 48 bytes for V=MinSig).
    pub bls_seed_partial: V::Signature,
    /// MinPk identity signature (96 bytes) that binds `bls_seed_partial` to the
    /// signer's identity key over `(round, vrf_material_version, partial)` under
    /// [`crate::proof::seed_attest_namespace`]. It makes the partial
    /// non-repudiably attributable, so a byzantine/equivocating partial is
    /// slashable (see [`crate::proof::seed_partial`]). It is NOT aggregated into
    /// the certificate and never reaches on-chain certificate bytes. It rides the
    /// per-vote P2P gossip only. It is consulted for attribution when quorum
    /// admission excludes the complete vote+partial pair.
    pub seed_partial_identity_sig: bls12381::Signature,
}

impl<V: Variant> Write for HybridSignature<V> {
    fn write(&self, writer: &mut impl BufMut) {
        self.bls_individual_vote.write(writer);
        writer.put_u64(self.vrf_material_version);
        self.bls_seed_partial.write(writer);
        self.seed_partial_identity_sig.write(writer);
    }
}

impl<V: Variant> Read for HybridSignature<V> {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &()) -> Result<Self, Error> {
        let bls_individual_vote = bls12381::Signature::read(reader)?;
        if reader.remaining() < 8 {
            return Err(Error::Invalid(
                "HybridSignature",
                "missing VRF material version",
            ));
        }
        let vrf_material_version = reader.get_u64();
        let bls_seed_partial = V::Signature::read(reader)?;
        let seed_partial_identity_sig = bls12381::Signature::read(reader)?;

        Ok(Self {
            bls_individual_vote,
            vrf_material_version,
            bls_seed_partial,
            seed_partial_identity_sig,
        })
    }
}

impl<V: Variant> FixedSize for HybridSignature<V> {
    const SIZE: usize =
        bls12381::Signature::SIZE + 8 + V::Signature::SIZE + bls12381::Signature::SIZE;
}

// Wire codec for `VrfProof` and `HybridCertificate` lives in `crate::proof::hybrid_wire`.
// This module re-exports both types below so existing call sites at
// `crate::hybrid::{VrfProof, HybridCertificate}` continue to compile and serialize
// byte-identically. There must be exactly one definition of each in the workspace.
pub use crate::proof::hybrid_wire::{HybridCertificate, VrfProof};

/// The committee binding shared by both roles: the ordered participant set, the
/// versioned VRF threshold material, and the pre-computed committee-bound
/// namespaces. These three always travel together. Embedding them as one value
/// makes that invariant structural, instead of re-stating it at every match
/// site. A `Signer` cannot carry a different shared-field shape than a
/// `Verifier`. The signer-only capability fields (`individual_key`, `index`) stay on
/// `Signer`, so "only a signer can sign" remains enforced by the variant.
#[derive(Clone, Debug)]
struct RoleFields<V: Variant> {
    /// Participants in the committee (BLS MinPk identity keys).
    participants: Set<bls12381::PublicKey>,
    /// Shared versioned VRF threshold material.
    vrf_materials: VrfMaterialProvider<V>,
    /// Immutable material version owned by this epoch-scoped scheme.
    expected_vrf_material_version: u64,
    /// Pre-computed namespaces.
    namespace: Namespace,
    /// Test-harness fault injection. Production schemes always leave this false.
    #[cfg(any(test, feature = "test-utils"))]
    invalid_seed_partial_for_test: bool,
}

/// The role-specific data for a hybrid scheme participant.
#[derive(Clone, Debug)]
enum Role<V: Variant> {
    Signer {
        /// Committee binding shared with the verifier role.
        fields: RoleFields<V>,
        /// BLS individual private key (MinPk) for signing votes.
        individual_key: bls12381::PrivateKey,
        /// Participant index in the ordered set.
        index: Participant,
    },
    Verifier {
        /// Committee binding shared with the signer role.
        fields: RoleFields<V>,
    },
}

/// Hybrid signing scheme: BLS MinPk individual attribution + BLS MinSig threshold VRF.
///
/// Identity keys are BLS MinPk (48-byte G1 pubkeys), used for participant ordering,
/// P2P identity, and aggregatable vote signatures.
/// BLS MinSig threshold keys provide VRF-based randomness for leader election.
#[derive(Clone, Debug)]
pub struct HybridScheme<V: Variant> {
    role: Role<V>,
    signing_gate: Option<signing::SigningGate>,
}

/// Build the per-scheme `Namespace` with the committee-bound vote
/// sub-namespaces:
/// - notarize/nullify/finalize bind `participant_set_commitment`, so an
///   individual vote cannot cross-verify under a different committee.
/// - seed stays chain-only. The threshold group key already makes it
///   committee-bound.
///
/// Both the signer and verifier constructors use this, so they sign and verify
/// votes under byte-identical namespaces. Every external verifier
/// (`proof::verifier`, `proof::late_finalize`, SlashIndicator evidence) derives
/// the same bytes from the same committee via `crate::proof::constants`.
fn committee_bound_namespace(
    base: &[u8],
    participants: &Set<bls12381::PublicKey>,
) -> commonware_consensus::simplex::scheme::Namespace {
    let mut ns = commonware_consensus::simplex::scheme::Namespace::new(base);
    ns.notarize = crate::proof::constants::notarize_namespace(participants);
    ns.nullify = crate::proof::constants::nullify_namespace(participants);
    ns.finalize = crate::proof::constants::finalize_namespace(participants);
    ns
}

impl<V: Variant> HybridScheme<V> {
    /// Creates a signer instance.
    ///
    /// * `namespace` - base namespace for domain separation
    /// * `participants` - ordered set of BLS MinPk identity keys
    /// * `individual_key` - BLS MinPk private key for this participant
    /// * `polynomial` - BLS MinSig public polynomial for threshold operations
    /// * `share` - BLS MinSig share for producing partial signatures
    ///
    /// Returns `None` if:
    /// - The BLS public key is not in the participant set
    /// - The BLS threshold share doesn't match the polynomial
    pub fn signer(
        namespace: &[u8],
        participants: Set<bls12381::PublicKey>,
        individual_key: bls12381::PrivateKey,
        polynomial: Sharing<V>,
        share: Share,
    ) -> Option<Self> {
        let vrf_materials = VrfMaterialProvider::new(0, polynomial, Some(share));
        Self::signer_with_vrf_provider(namespace, participants, individual_key, vrf_materials)
    }

    pub fn signer_with_vrf_provider(
        namespace: &[u8],
        participants: Set<bls12381::PublicKey>,
        individual_key: bls12381::PrivateKey,
        vrf_materials: VrfMaterialProvider<V>,
    ) -> Option<Self> {
        let expected_vrf_material_version = vrf_materials.active_version();
        if vrf_materials.polynomial_total(expected_vrf_material_version)? as usize
            != participants.len()
        {
            tracing::error!(
                polynomial_total = vrf_materials.polynomial_total(expected_vrf_material_version)?,
                participants = participants.len(),
                "polynomial total must equal participant count"
            );
            return None;
        }

        // Verify BLS individual key is in participants
        let bls_public_key = individual_key.public_key();
        let index = participants.index(&bls_public_key)?;

        // Verify share.index matches participant index.
        // If they diverge, threshold partial signatures will use the wrong
        // evaluation point and threshold::recover will fail.
        let share = vrf_materials.share(expected_vrf_material_version)?;
        if index != share.index {
            tracing::error!(
                participant_index = index.get(),
                share_index = share.index.get(),
                "share.index does not match participant index"
            );
            return None;
        }

        // Verify BLS threshold share matches polynomial
        let expected_public =
            vrf_materials.partial_public(expected_vrf_material_version, share.index)?;
        if expected_public != share.public::<V>() {
            return None;
        }

        let scheme_namespace = committee_bound_namespace(namespace, &participants);
        Some(Self {
            signing_gate: None,
            role: Role::Signer {
                fields: RoleFields {
                    participants,
                    vrf_materials,
                    expected_vrf_material_version,
                    namespace: scheme_namespace,
                    #[cfg(any(test, feature = "test-utils"))]
                    invalid_seed_partial_for_test: false,
                },
                individual_key,
                index,
            },
        })
    }

    /// Creates a verifier instance (cannot sign).
    ///
    /// Returns `None` if the polynomial total doesn't match participant count.
    pub fn verifier(
        namespace: &[u8],
        participants: Set<bls12381::PublicKey>,
        polynomial: Sharing<V>,
    ) -> Option<Self> {
        let vrf_materials = VrfMaterialProvider::new(0, polynomial, None);
        Self::verifier_with_vrf_provider(namespace, participants, vrf_materials)
    }

    pub fn verifier_with_vrf_provider(
        namespace: &[u8],
        participants: Set<bls12381::PublicKey>,
        vrf_materials: VrfMaterialProvider<V>,
    ) -> Option<Self> {
        let expected_vrf_material_version = vrf_materials.active_version();
        if vrf_materials.polynomial_total(expected_vrf_material_version)? as usize
            != participants.len()
        {
            tracing::error!(
                polynomial_total = vrf_materials.polynomial_total(expected_vrf_material_version)?,
                participants = participants.len(),
                "polynomial total must equal participant count"
            );
            return None;
        }

        let scheme_namespace = committee_bound_namespace(namespace, &participants);
        Some(Self {
            signing_gate: None,
            role: Role::Verifier {
                fields: RoleFields {
                    participants,
                    vrf_materials,
                    expected_vrf_material_version,
                    namespace: scheme_namespace,
                    #[cfg(any(test, feature = "test-utils"))]
                    invalid_seed_partial_for_test: false,
                },
            },
        })
    }

    /// Make this signer emit an identity-attributable seed partial over the
    /// wrong message. Used only by the real Simplex engine test harness.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn with_invalid_seed_partial_for_test(mut self) -> Self {
        if let Role::Signer { fields, .. } = &mut self.role {
            fields.invalid_seed_partial_for_test = true;
        }
        self
    }

    #[cfg(any(test, feature = "test-utils"))]
    fn invalid_seed_partial_for_test(&self) -> bool {
        match &self.role {
            Role::Signer { fields, .. } | Role::Verifier { fields } => {
                fields.invalid_seed_partial_for_test
            }
        }
    }

    /// The committee binding for this scheme, regardless of role. This is the
    /// single owner of the `Signer`/`Verifier` discrimination for shared-field
    /// access.
    fn fields(&self) -> &RoleFields<V> {
        match &self.role {
            Role::Signer { fields, .. } | Role::Verifier { fields, .. } => fields,
        }
    }

    fn participants_ref(&self) -> &Set<bls12381::PublicKey> {
        &self.fields().participants
    }

    fn vrf_materials(&self) -> &VrfMaterialProvider<V> {
        &self.fields().vrf_materials
    }

    fn namespace_ref(&self) -> &Namespace {
        &self.fields().namespace
    }

    /// Returns the public identity of the committee (BLS MinSig group public key).
    pub fn identity(&self) -> Option<V::Public> {
        self.vrf_materials()
            .public(self.expected_vrf_material_version())
    }

    /// Returns the immutable VRF material version pinned when this epoch scheme was built.
    pub fn expected_vrf_material_version(&self) -> u64 {
        self.fields().expected_vrf_material_version
    }

    pub fn verified_vrf_seed_for_round<R>(
        &self,
        _rng: &mut R,
        seed_round: Round,
        certificate: &HybridCertificate<V>,
        _strategy: &impl Strategy,
    ) -> Option<Vec<u8>>
    where
        R: CryptoRng,
    {
        let proof = &certificate.vrf_proof;
        if proof.material_version != self.expected_vrf_material_version() {
            return None;
        }
        // The consensus seed namespace is scheme-relative. It MUST match the
        // namespace that this scheme's signer used (`namespace_ref().seed`).
        // That namespace equals the global `hybrid_seed_namespace()` only when
        // the scheme is built with `outbe_app_namespace()` (production). The
        // proof-side global verifiers (`seed_partial`, `verifier`) use the
        // constant instead.
        let namespace = self.namespace_ref();
        let seed_message = seed_round.encode();
        if self
            .vrf_materials()
            .verify_proof(proof, &namespace.seed, seed_message.as_ref())
        {
            Some(proof.threshold_signature.encode().to_vec())
        } else {
            None
        }
    }

    pub fn verify_vrf_partial<R, D>(
        &self,
        _rng: &mut R,
        subject: Subject<'_, D>,
        attestation: &Attestation<Self>,
        _strategy: &impl Strategy,
    ) -> bool
    where
        R: CryptoRng,
        D: Digest,
    {
        let Some(signature) = attestation.signature.get() else {
            return false;
        };
        // Scheme-relative namespace (see `verified_vrf_seed_for_round`): match
        // this scheme's signer, not the global proof constant.
        let namespace = self.namespace_ref();
        let seed_message = seed_message_from_subject(&subject);
        self.vrf_materials().verify_partial(VrfPartialVerification {
            version: signature.vrf_material_version,
            signer: attestation.signer,
            namespace: &namespace.seed,
            seed_message: seed_message.as_ref(),
            signature: signature.bls_seed_partial,
        })
    }

    /// Verify that the seed-partial identity attestation rides correctly with the
    /// attestation. The result is `true` iff `seed_partial_identity_sig` is the
    /// MinPk identity signature of the participant at `attestation.signer` over
    /// `(round, vrf_material_version, bls_seed_partial)`.
    ///
    /// Deterministic (plain MinPk verify, no batch RNG), so every honest node
    /// reaches the same verdict. This is required because this drives slashing
    /// attribution. A `true` result proves the signer deliberately emitted this
    /// partial (not a relay forgery). It does NOT assert that the partial is valid.
    pub fn verify_seed_partial_identity_sig<D: Digest>(
        &self,
        subject: Subject<'_, D>,
        attestation: &Attestation<Self>,
    ) -> bool {
        let participants = self.participants_ref();
        let Some(public_key) = participants.key(attestation.signer) else {
            return false;
        };
        let Some(signature) = attestation.signature.get() else {
            return false;
        };
        let round = round_from_subject(&subject);
        let partial_bytes = signature.bls_seed_partial.encode();
        crate::proof::verify_seed_partial_attest(
            public_key,
            crate::proof::SeedPartialAttestation {
                round_epoch: round.epoch().get(),
                round_view: round.view().get(),
                vrf_material_version: signature.vrf_material_version,
                partial_bytes: partial_bytes.as_ref(),
            },
            &signature.seed_partial_identity_sig,
        )
    }

    /// Record a non-valid seed-partial verdict after quorum admission excludes
    /// the complete attestation. The signer is never returned through the
    /// p2p-invalid channel solely because of a partial problem.
    fn record_seed_partial_drop<D>(
        &self,
        subject: Subject<'_, D>,
        attestation: &Attestation<Self>,
        verdict: SeedPartialVerdict,
    ) where
        D: Digest,
    {
        let Some(signature) = attestation.signature.get() else {
            return;
        };
        let round = round_from_subject(&subject);
        crate::metrics::record_vrf_partial_drop(verdict.label(), attestation.signer.get());
        tracing::warn!(
            target: "outbe::hybrid::vrf_partial",
            verdict = verdict.label(),
            signer_index = attestation.signer.get(),
            epoch = round.epoch().get(),
            view = round.view().get(),
            expected_material_version = self.expected_vrf_material_version(),
            received_material_version = signature.vrf_material_version,
            "dropping vote and VRF partial from quorum admission"
        );

        match verdict {
            SeedPartialVerdict::Valid => {}
            SeedPartialVerdict::AttributableInvalid => {
                crate::metrics::record_invalid_vrf_partial();
                // Emit the attributable facts for an external slashing watcher to
                // pack into a SlashIndicator evidence transaction (see README
                // "Slashing"). The node reports facts, not packed wire. This keeps
                // the consensus crate independent of the evidence codec. The
                // identity signature makes "this signer emitted this partial"
                // non-repudiable and re-verifiable on chain from the committee
                // snapshot. So the watcher's submission cannot frame an honest
                // node.
                let signer_pubkey = self
                    .participants_ref()
                    .key(attestation.signer)
                    .map(|pk| hex::encode(pk.encode()))
                    .unwrap_or_default();
                tracing::warn!(
                    target: "outbe::slashing::seed_partial",
                    offense = "invalid_seed_partial",
                    signer_index = attestation.signer.get(),
                    signer_pubkey,
                    epoch = round.epoch().get(),
                    view = round.view().get(),
                    vrf_material_version = signature.vrf_material_version,
                    partial = hex::encode(signature.bls_seed_partial.encode()),
                    identity_sig = hex::encode(signature.seed_partial_identity_sig.encode()),
                    "attributable invalid VRF seed partial - slashable; external watcher should submit evidence"
                );
            }
            SeedPartialVerdict::Unattributable => {
                crate::metrics::record_forged_seed_partial();
            }
            SeedPartialVerdict::StaleVersion => {}
        }
    }

    /// Classify a seed partial for attestation verification / slashing
    /// attribution. The result is deterministic with respect to the identity-sig
    /// outcome. Every honest node reaches the same verdict on whether to
    /// attribute. This is required because the attributable verdict feeds
    /// slashing.
    pub fn classify_seed_partial<R, D>(
        &self,
        rng: &mut R,
        subject: Subject<'_, D>,
        attestation: &Attestation<Self>,
        strategy: &impl Strategy,
    ) -> SeedPartialVerdict
    where
        R: CryptoRng,
        D: Digest,
    {
        let Some(signature) = attestation.signature.get() else {
            return SeedPartialVerdict::Valid;
        };
        if signature.vrf_material_version != self.expected_vrf_material_version() {
            return SeedPartialVerdict::StaleVersion;
        }
        if self.verify_vrf_partial(rng, subject, attestation, strategy) {
            return SeedPartialVerdict::Valid;
        }
        if self.verify_seed_partial_identity_sig(subject, attestation) {
            SeedPartialVerdict::AttributableInvalid
        } else {
            SeedPartialVerdict::Unattributable
        }
    }
}

/// Verdict for a `bls_seed_partial` observed during attestation verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedPartialVerdict {
    /// Active-version partial that verifies against the committee polynomial.
    Valid,
    /// Partial tagged with a non-active material version. It is legitimately
    /// stale after a reshare. It is excluded from recovery and is not byzantine.
    StaleVersion,
    /// Active-version partial that fails verification, with a valid rider
    /// identity signature proving the signer authored it. It is slashable
    /// byzantine behavior.
    AttributableInvalid,
    /// Active-version partial that fails verification but whose rider identity
    /// signature does not verify. This is a probable relay forgery. Drop it and
    /// do not attribute it.
    Unattributable,
}

impl SeedPartialVerdict {
    const fn label(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::StaleVersion => "stale_version",
            Self::AttributableInvalid => "attributable_invalid",
            Self::Unattributable => "unattributable",
        }
    }
}

/// Extracts the consensus round from a Subject (the round the seed partial commits
/// to). This is the single Subject-matching core. The seed message is derived from
/// it (see [`seed_message_from_subject`]), so the round and its seed message can
/// never diverge.
fn round_from_subject<D: Digest>(subject: &Subject<'_, D>) -> Round {
    match subject {
        Subject::Notarize { proposal } | Subject::Finalize { proposal } => proposal.round,
        Subject::Nullify { round } => *round,
    }
}

/// The VRF seed message for a Subject: the canonical `Round::encode()` bytes of the
/// round the seed partial commits to. It is derived from [`round_from_subject`], so
/// it stays byte-identical to the proof-side recipe
/// (`proof::constants::seed_namespace_and_message`).
/// `seed_message_matches_proof_side_recipe` pins this.
fn seed_message_from_subject<D: Digest>(subject: &Subject<'_, D>) -> bytes::Bytes {
    round_from_subject(subject).encode()
}

impl<V: Variant> certificate::Verifier for HybridScheme<V> {
    type Subject<'a, D: Digest> = Subject<'a, D>;
    type Faults = N3f1;
    type PublicKey = bls12381::PublicKey;
    type Certificate = HybridCertificate<V>;

    fn verify_certificate<R, D>(
        &self,
        _rng: &mut R,
        subject: Subject<'_, D>,
        certificate: &Self::Certificate,
        _strategy: &impl Strategy,
    ) -> bool
    where
        R: CryptoRng,
        D: Digest,
    {
        let participants = self.participants_ref();
        let namespace = self.namespace_ref();

        // Structural checks
        if certificate.signers.len() != participants.len()
            || certificate.signers.count() < participants.quorum::<N3f1>() as usize
        {
            return false;
        }

        // 1. Verify aggregated BLS MinPk vote signature.
        //    Collect signer public keys, aggregate them, and verify.
        let vote_namespace = subject.namespace(namespace);
        let message = subject.message();

        let signer_pubkeys: Vec<&<MinPk as Variant>::Public> = certificate
            .signers
            .iter()
            .filter_map(|signer| participants.key(signer).map(|pk| pk.as_ref()))
            .collect();

        if signer_pubkeys.len() != certificate.signers.count() {
            return false;
        }

        let Some(signer_pubkeys) = NonEmpty::try_new(signer_pubkeys.into_iter()) else {
            return false;
        };
        let aggregate_pk = aggregate::combine_public_keys::<MinPk, _>(signer_pubkeys);
        if aggregate::verify_same_message::<MinPk>(
            &aggregate_pk,
            vote_namespace,
            &message,
            &certificate.bls_aggregated_vote,
        )
        .is_err()
        {
            return false;
        }

        let proof = &certificate.vrf_proof;
        if proof.material_version != self.expected_vrf_material_version() {
            return false;
        }
        let seed_message = seed_message_from_subject(&subject);
        self.vrf_materials()
            .verify_proof(proof, &namespace.seed, seed_message.as_ref())
    }

    fn is_batchable() -> bool {
        false
    }

    fn certificate_codec_config(&self) -> <Self::Certificate as Read>::Cfg {
        self.participants_ref().len()
    }

    fn certificate_codec_config_unbounded() -> <Self::Certificate as Read>::Cfg {
        u32::MAX as usize
    }
}

mod signing;

// Seed extraction helpers (`HybridCertificate::seed`, `raw_vrf_seed_bytes`) now
// live in `outbe_primitives::proof::hybrid_wire` alongside the type definition.

// ---------------------------------------------------------------------------
// SchemeProvider for HybridScheme
// ---------------------------------------------------------------------------

/// Epoch-scoped provider of hybrid schemes. Thin typed wrapper over a shared
/// [`EpochRegistry`](crate::epoch_registry::EpochRegistry). It keeps the
/// `certificate::Provider` impl, which the generic registry cannot carry.
#[derive(Clone, Debug)]
pub struct HybridSchemeProvider<V: Variant> {
    inner: crate::epoch_registry::EpochRegistry<HybridScheme<V>>,
}

impl<V: Variant> HybridSchemeProvider<V> {
    /// Create an empty provider.
    pub fn new() -> Self {
        Self {
            inner: crate::epoch_registry::EpochRegistry::new(),
        }
    }

    /// Register a scheme for the given epoch (insert-once).
    pub fn register(&self, epoch: Epoch, scheme: HybridScheme<V>) -> bool {
        self.inner.register(epoch, scheme)
    }

    /// Returns the pinned scheme for an epoch, preserving the application-facing API.
    pub fn scoped(&self, epoch: Epoch) -> Option<Arc<HybridScheme<V>>> {
        self.inner.get(&epoch)
    }

    /// Remove the scheme for the given epoch.
    pub fn remove(&self, epoch: &Epoch) -> bool {
        self.inner.remove(epoch)
    }
}

impl<V: Variant> Default for HybridSchemeProvider<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Variant> certificate::Provider for HybridSchemeProvider<V> {
    type Scope = Epoch;
    type Scheme = HybridScheme<V>;

    fn scoped(&self, scope: Self::Scope) -> Option<certificate::Scoped<Self::Scheme>> {
        // Every registered HybridScheme retains its epoch's participant material,
        // including verifier-only instances. Marshal needs that full scheme for
        // notarized payload checks; me()/sign() still enforce local signing ability.
        self.inner.get(&scope).map(certificate::Scoped::scheme)
    }
}

#[cfg(test)]
mod tests;
