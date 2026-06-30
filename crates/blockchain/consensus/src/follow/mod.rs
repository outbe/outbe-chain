//! Lightweight follower: cold-sync finalized blocks from an upstream node and
//! verify them against the chain's committee, WITHOUT running consensus.
//!
//! **Trust model — committee-chaining.** outbe's finalize certificate is an
//! aggregate of individual MinPk votes over a *committee-bound* namespace, so a
//! single group key cannot verify it (unlike Tempo); the verifier needs the
//! exact per-epoch committee, which changes on every reshare. A follower
//! therefore:
//!
//! 1. anchors the START epoch's committee on a single trusted
//!    [`NetworkIdentity`](crate::network_identity::NetworkIdentity) (the group
//!    public key) — the trust root;
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
use commonware_cryptography::bls12381::primitives::variant::{MinSig, Variant};
use commonware_parallel::Sequential;
use commonware_utils::ordered::Set;
use eyre::{bail, Result};

use crate::digest::Digest;
use crate::hybrid::{bls_batch_verification_rng, HybridScheme, HybridSchemeProvider};
use crate::network_identity::NetworkIdentity;

/// Builds and chains per-epoch finalization verifiers from finalized boundary
/// blocks, anchored on a trusted [`NetworkIdentity`]. Verifiers are kept in a
/// [`HybridSchemeProvider`] keyed by epoch — the same provider type the live
/// stack uses — so cert verification is byte-identical to the validator path.
pub struct CommitteeChain {
    anchor: NetworkIdentity,
    scheme_provider: HybridSchemeProvider<MinSig>,
    /// Highest epoch whose committee verifier has been registered.
    highest_registered: Option<Epoch>,
}

impl CommitteeChain {
    /// Create a chain anchored on `anchor` (the trusted group key + start epoch).
    pub fn new(anchor: NetworkIdentity) -> Self {
        Self {
            anchor,
            scheme_provider: HybridSchemeProvider::new(),
            highest_registered: None,
        }
    }

    /// The epoch the anchor is rooted at (the first epoch the follower can verify).
    pub fn anchor_epoch(&self) -> u64 {
        self.anchor.from_epoch
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

        // Trust root: the anchor epoch's committee must hash to the trusted key.
        if epoch.get() == self.anchor.from_epoch {
            let group_key: &<MinSig as Variant>::Public = polynomial.public();
            if group_key != &self.anchor.identity {
                bail!(
                    "anchor mismatch: start-epoch {} committee group key {} does not match \
                     trusted network identity {}",
                    epoch.get(),
                    hex::encode(commonware_codec::Encode::encode(group_key)),
                    hex::encode(commonware_codec::Encode::encode(&self.anchor.identity)),
                );
            }
        }

        let verifier = HybridScheme::<MinSig>::verifier(
            &crate::config::outbe_app_namespace(),
            participants.clone(),
            polynomial,
        )
        .ok_or_else(|| eyre::eyre!("failed to build committee verifier for epoch {}", epoch.get()))?;
        self.scheme_provider.register(epoch, verifier);
        self.highest_registered = Some(match self.highest_registered {
            Some(h) => h.max(epoch),
            None => epoch,
        });
        Ok(participants)
    }

    /// Verify a finalization certificate for `epoch` against its registered
    /// committee verifier. Errors if no verifier is registered for `epoch` or the
    /// certificate fails verification.
    pub fn verify_finalization(
        &self,
        epoch: Epoch,
        finalization: &Finalization<HybridScheme<MinSig>, Digest>,
    ) -> Result<()> {
        let scheme = commonware_cryptography::certificate::Provider::scoped(
            &self.scheme_provider,
            epoch,
        )
        .ok_or_else(|| eyre::eyre!("no committee verifier registered for epoch {}", epoch.get()))?;
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

    /// One committee for `epoch`: returns its boundary `outcome` bytes (public,
    /// as on-chain) plus a verifiable finalization signed by the committee.
    fn committee_epoch(
        epoch: Epoch,
        seed_base: u8,
    ) -> (
        Vec<u8>,
        Finalization<HybridScheme<MinSig>, Digest>,
        <MinSig as Variant>::Public,
    ) {
        let mut keys: Vec<bls12381::PrivateKey> = (0..4u8)
            .map(|i| bls12381::PrivateKey::from_seed((seed_base + i + 1) as u64))
            .collect();
        keys.sort_by_key(|k| k.public_key().encode());
        let participants: OrderedSet<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();
        let dkg = crate::bls::bootstrap_dkg_for_participants(participants.clone()).unwrap();
        let group_key = dkg.polynomial.public().clone();

        // Public boundary outcome bytes (what the chain writes into the block).
        let outcome = crate::dkg_manager::encode_outcome(epoch, &dkg.output, false).to_vec();

        // A finalization signed by the committee.
        let ns = crate::config::outbe_app_namespace();
        let verifier =
            HybridScheme::<MinSig>::verifier(&ns, participants.clone(), dkg.polynomial.clone())
                .unwrap();
        let signers: Vec<HybridScheme<MinSig>> = keys
            .iter()
            .map(|key| {
                let idx = participants.index(&key.public_key()).unwrap();
                HybridScheme::signer(
                    &ns,
                    participants.clone(),
                    key.clone(),
                    dkg.polynomial.clone(),
                    dkg.shares[idx.get() as usize].clone(),
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
        let certificate = verifier.assemble::<_, N3f1>(attestations, &Sequential).unwrap();
        (
            outcome,
            Finalization {
                proposal,
                certificate,
            },
            group_key,
        )
    }

    #[test]
    fn committee_chain_anchors_then_chains_across_epochs() {
        let e5 = Epoch::new(5);
        let e6 = Epoch::new(6);
        let (outcome5, fin5, group5) = committee_epoch(e5, 10);
        let (outcome6, fin6, _group6) = committee_epoch(e6, 50);

        // Anchor on epoch 5's group key.
        let anchor = NetworkIdentity {
            from_epoch: 5,
            identity: group5,
        };
        let mut chain = CommitteeChain::new(anchor);

        // Register + verify the anchor epoch.
        chain.register_epoch_from_outcome(e5, &outcome5).unwrap();
        chain.verify_finalization(e5, &fin5).unwrap();

        // Chain forward to epoch 6 (a different committee) and verify it.
        chain.register_epoch_from_outcome(e6, &outcome6).unwrap();
        chain.verify_finalization(e6, &fin6).unwrap();
        assert_eq!(chain.highest_registered(), Some(e6));

        // A finalization can't be verified for an unregistered epoch.
        assert!(chain.verify_finalization(Epoch::new(7), &fin6).is_err());
    }

    #[test]
    fn committee_chain_rejects_anchor_mismatch() {
        let e5 = Epoch::new(5);
        let (outcome5, _fin5, _group5) = committee_epoch(e5, 10);
        // Anchor on a DIFFERENT committee's group key.
        let (_o, _f, wrong_group) = committee_epoch(e5, 99);
        let mut chain = CommitteeChain::new(NetworkIdentity {
            from_epoch: 5,
            identity: wrong_group,
        });
        let err = chain
            .register_epoch_from_outcome(e5, &outcome5)
            .unwrap_err()
            .to_string();
        assert!(err.contains("anchor mismatch"), "error: {err}");
    }
}
