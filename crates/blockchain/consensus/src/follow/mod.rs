//! Lightweight follower: cold-sync finalized blocks from an upstream node and
//! verify them against the chain's committee, WITHOUT running consensus.
//!
//! **Trust model — committee-chaining.** outbe's finalize certificate is an
//! aggregate of individual MinPk votes over a *committee-bound* namespace
//! (the MinSig VRF group key is an optional seed sidecar, NOT required for
//! finality), so finality is authenticated by the committee's MinPk key set,
//! which changes on every reshare. A follower therefore:
//!
//! 1. anchors the START epoch's committee on the **genesis validator MinPk
//!    set**, read from the follower's OWN genesis state — the trust root;
//!    nothing the operator must provide;
//! 2. reads each later epoch's committee from the finalized **boundary block**
//!    that activates it, and trusts it transitively because that boundary block
//!    was finalized by the already-trusted previous committee.
//!
//! All inputs are public on-chain data carried in the boundary block
//! `extra_data` (the full DKG [`Output`] — players + polynomial); the follower
//! never holds any DKG secret. [`CommitteeChain`] implements this chaining; it
//! is exercised by `phase0_spike_*` (the de-risk gate) and the tests below.

use commonware_consensus::{simplex::types::Finalization, types::Epoch};
use commonware_cryptography::bls12381;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_parallel::Sequential;
use commonware_utils::ordered::Set;
use eyre::{bail, Result};

use crate::digest::Digest;
use crate::hybrid::{bls_batch_verification_rng, HybridScheme, HybridSchemeProvider};

mod driver;
pub mod engine;
mod epocher;
mod resolver;
mod stubs;
pub mod upstream;

pub use engine::{run_follow_engine, FollowEngineConfig};
pub use epocher::FollowerEpocher;
pub use upstream::{CertifiedFinalizedBlock, FinalizedSource, LocalBlockSource, TipSource};

/// Builds and chains per-epoch finalization verifiers from finalized boundary
/// blocks, anchored on the trusted genesis committee. Verifiers are kept in a
/// [`HybridSchemeProvider`] keyed by epoch — the same provider type the live
/// stack uses — so cert verification is byte-identical to the validator path.
///
/// **Trust root.** Consensus finality is a multisig over the committee's
/// individual MinPk keys (the MinSig VRF group key is an optional seed sidecar,
/// NOT the consensus authenticator). So the anchor is the **genesis validator
/// MinPk set**, read from the follower's OWN genesis state — not a VRF group
/// key, and nothing the operator has to provide. The start epoch's committee
/// (`output.players()`) must equal this set; each later epoch's committee is
/// trusted via the finalized-boundary chain.
pub struct CommitteeChain {
    /// The start epoch the anchor is rooted at (genesis = 0).
    anchor_epoch: Epoch,
    /// The trusted start-epoch committee: the genesis validator MinPk keys.
    anchor_participants: Set<bls12381::PublicKey>,
    scheme_provider: HybridSchemeProvider<MinSig>,
    /// Highest epoch whose committee verifier has been registered.
    highest_registered: Option<Epoch>,
}

impl CommitteeChain {
    /// Create a chain anchored on the trusted genesis committee
    /// (`anchor_participants` = the genesis validator MinPk set, read from the
    /// follower's genesis state) at `anchor_epoch` (0 for a genesis anchor).
    pub fn new(anchor_epoch: Epoch, anchor_participants: Set<bls12381::PublicKey>) -> Self {
        Self {
            anchor_epoch,
            anchor_participants,
            scheme_provider: HybridSchemeProvider::new(),
            highest_registered: None,
        }
    }

    /// The epoch the anchor is rooted at (the first epoch the follower can verify).
    pub fn anchor_epoch(&self) -> u64 {
        self.anchor_epoch.get()
    }

    /// The per-epoch verifier provider, ready to hand to cert-verification paths.
    pub fn scheme_provider(&self) -> &HybridSchemeProvider<MinSig> {
        &self.scheme_provider
    }

    /// Highest epoch whose verifier is registered, if any.
    pub fn highest_registered(&self) -> Option<Epoch> {
        self.highest_registered
    }

    /// Register epoch `epoch`'s committee verifier from its finalized boundary
    /// `outcome` bytes (the ODKO-wrapped DKG output in the boundary block's
    /// `extra_data`).
    ///
    /// For the anchor epoch (`epoch == anchor.from_epoch`) the committee's group
    /// key MUST equal the trusted anchor identity — this is the trust root. For
    /// later epochs the caller is responsible for only registering committees
    /// from boundary blocks it has already verified as finalized by the prior
    /// (trusted) committee (the chaining link).
    ///
    /// Returns the epoch's ordered participant set.
    pub fn register_epoch_from_outcome(
        &mut self,
        epoch: Epoch,
        outcome: &[u8],
    ) -> Result<Set<bls12381::PublicKey>> {
        let output = crate::dkg_manager::decode_boundary_outcome(outcome)
            .ok_or_else(|| eyre::eyre!("boundary outcome is not a decodable full DKG output"))?;
        let participants = output.players().clone();
        let polynomial = output.public().clone();

        // Trust root: the anchor epoch's committee MUST be the trusted genesis
        // validator set. Consensus finality is a multisig over these MinPk keys,
        // so matching the participant set (NOT the VRF group key) authenticates
        // the committee. Compare as ordered sets (both pubkey-sorted).
        if epoch == self.anchor_epoch && participants != self.anchor_participants {
            bail!(
                "anchor mismatch: start-epoch {} committee ({} validators) does not match the \
                 trusted genesis validator set ({} validators)",
                epoch.get(),
                participants.len(),
                self.anchor_participants.len(),
            );
        }

        let verifier = HybridScheme::<MinSig>::verifier(
            &crate::config::outbe_app_namespace(),
            participants.clone(),
            polynomial,
        )
        .ok_or_else(|| {
            eyre::eyre!(
                "failed to build committee verifier for epoch {}",
                epoch.get()
            )
        })?;
        self.scheme_provider.register(epoch, verifier);
        self.highest_registered = Some(match self.highest_registered {
            Some(h) => h.max(epoch),
            None => epoch,
        });
        Ok(participants)
    }

    /// Advance the chain from a finalized block's `extra_data`, registering an
    /// epoch's committee verifier (the forward-chaining step). Returns the
    /// registered epoch, if any.
    ///
    /// Two carriers register a committee:
    /// - [`CommitteePreAnnounce`](outbe_primitives::reshare_artifact::ConsensusHeaderArtifact::CommitteePreAnnounce)
    ///   — the Path A committee-chaining carrier: epoch `E`'s committee riding a
    ///   block finalized by the already-trusted `E-1` committee. This is the
    ///   authenticated path — the trust chains from genesis through each E-1.
    /// - [`BoundaryOutcome`](outbe_primitives::reshare_artifact::ConsensusHeaderArtifact::BoundaryOutcome)
    ///   — the activating boundary at `E·L+1`, finalized by `E` ITSELF. We register
    ///   from it ONLY for a not-yet-known epoch (the genesis anchor; and, until the
    ///   pre-announce producer is wired, epochs lacking a pre-announce). We must NOT
    ///   let it OVERRIDE a committee already registered via its `E-1` pre-announce:
    ///   a self-finalized boundary overriding the chained committee is exactly the
    ///   D1 self-certification bug.
    ///
    /// Safe only for `extra_data` from blocks already verified as finalized by the
    /// trusted committee (the marshal enforces this via its `provider`), so the
    /// registered committee inherits that trust.
    pub fn advance_from_block_extra_data(&mut self, extra_data: &[u8]) -> Result<Option<Epoch>> {
        use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact as CHA;
        let artifacts =
            outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(extra_data)
                .map_err(|e| eyre::eyre!("failed to decode block artifacts: {e:?}"))?;
        match artifacts.consensus_header_artifact {
            Some(CHA::CommitteePreAnnounce { epoch, outcome }) => {
                let epoch = Epoch::new(epoch);
                self.register_epoch_from_outcome(epoch, &outcome)?;
                Ok(Some(epoch))
            }
            Some(CHA::BoundaryOutcome(boundary)) => {
                let epoch = Epoch::new(boundary.epoch);
                if self
                    .highest_registered
                    .is_none_or(|h| epoch.get() > h.get())
                {
                    self.register_epoch_from_outcome(epoch, &boundary.outcome)?;
                    Ok(Some(epoch))
                } else {
                    // Already registered — via the E-1 pre-announce (the trusted
                    // chaining path). The self-finalized boundary is a no-op here; it
                    // must NOT override the chained committee (that would be D1).
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    /// Verify a finalization certificate for `epoch` against its registered
    /// committee verifier. Errors if no verifier is registered for `epoch` or the
    /// certificate fails verification.
    pub fn verify_finalization(
        &self,
        epoch: Epoch,
        finalization: &Finalization<HybridScheme<MinSig>, Digest>,
    ) -> Result<()> {
        let scheme =
            commonware_cryptography::certificate::Provider::scoped(&self.scheme_provider, epoch)
                .ok_or_else(|| {
                    eyre::eyre!("no committee verifier registered for epoch {}", epoch.get())
                })?;
        let mut rng = bls_batch_verification_rng();
        if !finalization.verify(&mut rng, scheme.as_ref(), &Sequential) {
            bail!(
                "finalization certificate failed verification for epoch {}",
                epoch.get()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_codec::Encode as _;
    use commonware_consensus::simplex::types::{Finalization, Proposal, Subject};
    use commonware_consensus::types::{Round, View};
    use commonware_cryptography::certificate::Scheme as _;
    use commonware_cryptography::{Hasher as _, Sha256, Signer as _};
    use commonware_utils::{
        ordered::{Quorum as _, Set as OrderedSet},
        N3f1, TryCollect as _,
    };

    /// A single committee + its DKG, used to build BOTH a boundary block's
    /// `extra_data` and a matching finalization signed by that committee. (The
    /// DKG dealing is randomized, so the boundary and the finalization MUST come
    /// from the same `Committee`.)
    struct Committee {
        keys: Vec<bls12381::PrivateKey>,
        participants: OrderedSet<bls12381::PublicKey>,
        dkg: crate::bls::ParticipantDkgBootstrapResult,
    }

    fn committee(seed_base: u8) -> Committee {
        let mut keys: Vec<bls12381::PrivateKey> = (0..4u8)
            .map(|i| bls12381::PrivateKey::from_seed((seed_base + i + 1) as u64))
            .collect();
        keys.sort_by_key(|k| k.public_key().encode());
        let participants: OrderedSet<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();
        let dkg = crate::bls::bootstrap_dkg_for_participants(participants.clone()).unwrap();
        Committee {
            keys,
            participants,
            dkg,
        }
    }

    impl Committee {
        /// The public boundary `outcome` bytes (the ODKO DKG output).
        fn outcome(&self, epoch: Epoch) -> Vec<u8> {
            crate::dkg_manager::encode_outcome(epoch, &self.dkg.output, false).to_vec()
        }

        /// A full boundary block's `extra_data` carrying this committee's outcome.
        fn boundary_block_extra_data(&self, epoch: Epoch) -> Vec<u8> {
            use outbe_primitives::reshare_artifact::{
                encode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
            };
            use outbe_primitives::validators::ValidatorP2pAddress;
            let vs = crate::validators::ValidatorSet {
                public_keys: self.participants.iter().cloned().collect(),
                addresses: (0..self.participants.len() as u8)
                    .map(|i| alloy_primitives::Address::repeat_byte(i + 1))
                    .collect(),
                p2p_addresses: vec![ValidatorP2pAddress::Missing; self.participants.len()],
            };
            let artifact = crate::dkg_manager::build_boundary_artifact(
                crate::dkg_manager::BoundaryArtifactInput {
                    epoch,
                    validator_set: &vs,
                    output: &self.dkg.output,
                    is_full_dkg: false,
                    dkg_cycle: 1,
                    freeze_height: 100,
                    planned_activation_height: 120,
                    vrf_material_version: 1,
                    is_validator_set_change: false,
                    tee_reshare_registrations: vec![],
                },
            )
            .unwrap();
            encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)),
                ..Default::default()
            })
            .unwrap()
            .to_vec()
        }

        /// An `E-1`-finalized block's `extra_data` pre-announcing this committee for
        /// `epoch` (the Path A committee-chaining carrier).
        fn preannounce_block_extra_data(&self, epoch: Epoch) -> Vec<u8> {
            use outbe_primitives::reshare_artifact::{
                encode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
            };
            encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                consensus_header_artifact: Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
                    epoch: epoch.get(),
                    outcome: alloy_primitives::Bytes::from(self.outcome(epoch)),
                }),
                ..Default::default()
            })
            .unwrap()
            .to_vec()
        }

        /// A finalization for `epoch` signed by this committee.
        fn finalization(&self, epoch: Epoch) -> Finalization<HybridScheme<MinSig>, Digest> {
            let ns = crate::config::outbe_app_namespace();
            let verifier = HybridScheme::<MinSig>::verifier(
                &ns,
                self.participants.clone(),
                self.dkg.polynomial.clone(),
            )
            .unwrap();
            let signers: Vec<HybridScheme<MinSig>> = self
                .keys
                .iter()
                .map(|key| {
                    let idx = self.participants.index(&key.public_key()).unwrap();
                    HybridScheme::signer(
                        &ns,
                        self.participants.clone(),
                        key.clone(),
                        self.dkg.polynomial.clone(),
                        self.dkg.shares[idx.get() as usize].clone(),
                    )
                    .unwrap()
                })
                .collect();
            let digest = Digest::from(alloy_primitives::B256::from_slice(
                Sha256::hash(format!("blk-{}", epoch.get()).as_bytes()).as_ref(),
            ));
            let proposal = Proposal::new(Round::new(epoch, View::new(2)), View::new(1), digest);
            let subject = Subject::Finalize {
                proposal: &proposal,
            };
            let attestations: Vec<_> = signers
                .iter()
                .map(|s| s.sign::<Digest>(subject).unwrap())
                .collect();
            let certificate = verifier
                .assemble::<_, N3f1>(attestations, &Sequential)
                .unwrap();
            Finalization {
                proposal,
                certificate,
            }
        }
    }

    #[test]
    fn committee_chain_anchors_then_chains_across_epochs() {
        let (e5, e6) = (Epoch::new(5), Epoch::new(6));
        let c5 = committee(10);
        let c6 = committee(50);
        let mut chain = CommitteeChain::new(e5, c5.participants.clone());

        chain
            .register_epoch_from_outcome(e5, &c5.outcome(e5))
            .unwrap();
        chain.verify_finalization(e5, &c5.finalization(e5)).unwrap();

        // Chain forward to epoch 6 (a different committee) and verify it.
        chain
            .register_epoch_from_outcome(e6, &c6.outcome(e6))
            .unwrap();
        chain.verify_finalization(e6, &c6.finalization(e6)).unwrap();
        assert_eq!(chain.highest_registered(), Some(e6));

        // A finalization can't be verified for an unregistered epoch.
        assert!(chain
            .verify_finalization(Epoch::new(7), &c6.finalization(e6))
            .is_err());
    }

    #[test]
    fn preannounce_registers_and_self_finalized_boundary_cannot_override() {
        // The D1 fix, end to end at the follower: epoch 6's committee is registered
        // from its E-1 PRE-ANNOUNCE (carried in a block finalized by the trusted
        // epoch-5 committee — the chained path). A later self-finalized epoch-6
        // boundary announcing a DIFFERENT (forged) committee must NOT override it.
        let (e5, e6) = (Epoch::new(5), Epoch::new(6));
        let c5 = committee(10);
        let c6 = committee(50); // the real epoch-6 committee, pre-announced by trusted e5
        let forged6 = committee(77); // what a malicious self-finalized e6 boundary would claim
        let mut chain = CommitteeChain::new(e5, c5.participants.clone());
        chain
            .register_epoch_from_outcome(e5, &c5.outcome(e5))
            .unwrap();

        // Pre-announce epoch 6 in an e5-finalized block -> registered (chained trust).
        let pre6 = c6.preannounce_block_extra_data(e6);
        assert_eq!(
            chain.advance_from_block_extra_data(&pre6).unwrap(),
            Some(e6)
        );
        chain.verify_finalization(e6, &c6.finalization(e6)).unwrap();

        // A forged, self-finalized epoch-6 boundary is a NO-OP — it cannot overwrite
        // the chained committee (that overwrite would be the D1 bug).
        let forged_boundary = forged6.boundary_block_extra_data(e6);
        assert_eq!(
            chain
                .advance_from_block_extra_data(&forged_boundary)
                .unwrap(),
            None
        );
        // The forged committee's finalization is rejected; the real one still verifies.
        assert!(chain
            .verify_finalization(e6, &forged6.finalization(e6))
            .is_err());
        chain.verify_finalization(e6, &c6.finalization(e6)).unwrap();
    }

    #[test]
    fn committee_chain_rejects_anchor_mismatch() {
        let e5 = Epoch::new(5);
        let c5 = committee(10);
        let wrong = committee(99);
        let mut chain = CommitteeChain::new(e5, wrong.participants.clone());
        let err = chain
            .register_epoch_from_outcome(e5, &c5.outcome(e5))
            .unwrap_err()
            .to_string();
        assert!(err.contains("anchor mismatch"), "error: {err}");
    }

    #[test]
    fn committee_chain_advances_from_boundary_block_extra_data() {
        let e6 = Epoch::new(6);
        let c6 = committee(70);
        // Anchor on epoch 6 — the boundary block we process announces it.
        let mut chain = CommitteeChain::new(e6, c6.participants.clone());
        // Feeding the boundary block's extra_data registers epoch 6's committee.
        let extra = c6.boundary_block_extra_data(e6);
        assert_eq!(
            chain.advance_from_block_extra_data(&extra).unwrap(),
            Some(e6)
        );
        // That epoch's finalization now verifies.
        chain.verify_finalization(e6, &c6.finalization(e6)).unwrap();
        // A non-boundary block (empty extra_data) registers nothing.
        assert_eq!(chain.advance_from_block_extra_data(&[]).unwrap(), None);
    }

    /// The follow resolver serves a `Request::Finalized` delivery as the
    /// finalization certificate bytes immediately followed by the block bytes.
    /// The marshal decodes that exact layout by reading the `Finalization` with
    /// the epoch verifier's certificate codec config, then decoding the
    /// `ConsensusBlock` from the REMAINING buffer. This pins that two-step decode
    /// against the resolver's `finalization.encode() ++ block.encode()` wire
    /// format — the load-bearing interop contract between the follower's
    /// resolver and the marshal (a divergence here would compile clean but fail
    /// every backfill at runtime).
    #[test]
    fn finalized_delivery_wire_format_round_trips() {
        use crate::block::ConsensusBlock;
        use commonware_codec::Read as _;
        use commonware_cryptography::certificate::Scheme as _;

        let epoch = Epoch::new(3);
        let c = committee(20);

        // A certificate the marshal will decode with this verifier's config.
        let finalization = c.finalization(epoch);
        let verifier = HybridScheme::<MinSig>::verifier(
            &crate::config::outbe_app_namespace(),
            c.participants.clone(),
            c.dkg.polynomial.clone(),
        )
        .unwrap();
        let cert_cfg = verifier.certificate_codec_config();

        // An arbitrary valid block (its digest need not match the finalization
        // payload for the codec contract — the marshal checks that separately).
        let block = {
            use alloy_primitives::Bytes;
            use outbe_primitives::OutbeHeader;
            use reth_ethereum::primitives::SealedBlock;
            use reth_ethereum::Block;
            let mut b = Block::default();
            b.header.number = 42;
            b.header.extra_data = Bytes::from_static(b"wire-fmt");
            let b = b.map_header(OutbeHeader::new);
            ConsensusBlock::from_sealed(SealedBlock::seal_slow(b))
        };

        // Exactly what `resolver::resolve_one` builds for a Finalized delivery.
        let mut wire = finalization.encode().to_vec();
        wire.extend_from_slice(block.encode().as_ref());

        // Decode the marshal's way: certificate first (with its cfg), block from
        // the remaining bytes.
        let mut buf: &[u8] = &wire;
        let decoded_fin =
            Finalization::<HybridScheme<MinSig>, Digest>::read_cfg(&mut buf, &cert_cfg)
                .expect("finalization must decode from the delivery prefix");
        let decoded_block = ConsensusBlock::read_cfg(&mut buf, &())
            .expect("block must decode from the delivery suffix");

        assert_eq!(
            decoded_fin.proposal.payload, finalization.proposal.payload,
            "decoded finalization payload must match"
        );
        assert_eq!(
            decoded_block.digest(),
            block.digest(),
            "decoded block digest must match the served block"
        );
        assert!(
            buf.is_empty(),
            "the delivery buffer must be fully consumed (cert ++ block, nothing trailing)"
        );
    }

    /// Full `outbe_getFinalization` server→client interop. The SERVER side
    /// (drainer) encodes the certificate and block separately and hexes them
    /// (`FinalizedBlockBytes` → `FinalizationProof`); the CLIENT side hex-decodes
    /// and decodes the certificate with the UNBOUNDED committee config (the
    /// engine `UpstreamRpcClient` path — it has no committee size yet), then the
    /// follower registers the epoch committee from the boundary block and the
    /// marshal-equivalent verification passes. This pins that:
    ///   (a) the unbounded cfg decodes a real committee-length certificate, and
    ///   (b) the decoded `(finalization, block)` is exactly what the resolver
    ///       registers + the `CommitteeChain` verifies — i.e. a follower accepts
    ///       what a validator serves, end to end.
    #[test]
    fn served_finalization_round_trips_to_verified_certified_block() {
        use crate::block::ConsensusBlock;
        use commonware_codec::Read as _;
        use commonware_cryptography::certificate::Scheme as _;

        let epoch = Epoch::new(4);
        let c = committee(40);

        // Anchor a chain on this committee and register epoch 4 from its boundary
        // block — exactly what the follower does on the fetch path.
        let mut chain = CommitteeChain::new(epoch, c.participants.clone());
        let boundary_extra = c.boundary_block_extra_data(epoch);
        assert_eq!(
            chain
                .advance_from_block_extra_data(&boundary_extra)
                .unwrap(),
            Some(epoch)
        );

        // SERVER: encode cert + block separately (the drainer's FinalizedBlockBytes)
        // and hex them (the FinalizationProof shipped over RPC).
        let finalization = c.finalization(epoch);
        let block = {
            use alloy_primitives::Bytes;
            use outbe_primitives::OutbeHeader;
            use reth_ethereum::primitives::SealedBlock;
            use reth_ethereum::Block;
            let mut b = Block::default();
            b.header.number = 4;
            b.header.extra_data = Bytes::from(boundary_extra.clone());
            let b = b.map_header(OutbeHeader::new);
            ConsensusBlock::from_sealed(SealedBlock::seal_slow(b))
        };
        let finalization_hex = format!("0x{}", hex::encode(finalization.encode()));
        let block_hex = format!("0x{}", hex::encode(block.encode()));

        // CLIENT: hex-decode and decode the certificate with the UNBOUNDED
        // committee config (the engine UpstreamRpcClient path).
        let fin_bytes = hex::decode(finalization_hex.trim_start_matches("0x")).unwrap();
        let block_bytes = hex::decode(block_hex.trim_start_matches("0x")).unwrap();
        let unbounded_cfg = HybridScheme::<MinSig>::certificate_codec_config_unbounded();
        let mut fin_reader: &[u8] = &fin_bytes;
        let decoded_fin =
            Finalization::<HybridScheme<MinSig>, Digest>::read_cfg(&mut fin_reader, &unbounded_cfg)
                .expect("client must decode the served finalization with the unbounded cfg");
        assert!(
            fin_reader.is_empty(),
            "no trailing bytes after finalization"
        );
        let mut block_reader: &[u8] = &block_bytes;
        let _decoded_block = ConsensusBlock::read_cfg(&mut block_reader, &())
            .expect("client must decode the served block");
        assert!(block_reader.is_empty(), "no trailing bytes after block");

        // The decoded certificate verifies against the committee the follower
        // registered from the boundary block — a follower accepts what the
        // validator served.
        chain
            .verify_finalization(epoch, &decoded_fin)
            .expect("the round-tripped certificate must verify against the registered committee");
    }
}
