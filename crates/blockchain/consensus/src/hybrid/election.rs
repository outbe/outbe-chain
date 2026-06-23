//! VRF-based leader election for the hybrid consensus scheme.
//!
//! Lifted out of `hybrid.rs`: `HybridRandom` (elector config), the
//! `HybridRandomElector` it builds, and the epoch-scoped
//! `HybridElectorConfigProvider`. The dependency is one-way — election consumes
//! `HybridScheme` / `HybridCertificate` / `VrfMaterialProvider` from the parent
//! module; the scheme/materials never reference election.

use commonware_codec::Encode;
use commonware_consensus::simplex::elector;
use commonware_consensus::types::{Epoch, Round};
use commonware_cryptography::bls12381::{
    self,
    primitives::variant::{MinSig, Variant},
};
use commonware_utils::{modulo, ordered::Set, Participant};
use std::sync::Arc;

use super::{bls_batch_verification_rng, HybridCertificate, HybridScheme, VrfMaterialProvider};

/// Configuration for hybrid VRF-based leader election.
///
/// Uses the BLS seed from `HybridCertificate` for unpredictable leader selection.
/// The very first produced view after chain genesis has no previous certificate and
/// therefore falls back to round-robin. Epoch restarts can provide a bootstrap seed
/// from the last finalized certificate of the previous epoch so that view 1 of later
/// epochs keeps using VRF-derived leader selection.
#[derive(Clone, Debug)]
pub struct HybridRandom<V: Variant = MinSig> {
    bootstrap_seed: Option<Vec<u8>>,
    vrf_materials: Option<VrfMaterialProvider<V>>,
}

impl<V: Variant> Default for HybridRandom<V> {
    fn default() -> Self {
        Self {
            bootstrap_seed: None,
            vrf_materials: None,
        }
    }
}

impl<V: Variant> HybridRandom<V> {
    pub fn with_vrf_materials(vrf_materials: VrfMaterialProvider<V>) -> Self {
        Self {
            bootstrap_seed: None,
            vrf_materials: Some(vrf_materials),
        }
    }

    /// Use a previous finalized certificate's seed bytes to bootstrap view 1
    /// leader selection for a newly started epoch.
    pub fn with_bootstrap_seed(seed: Vec<u8>) -> Self {
        Self {
            bootstrap_seed: Some(seed),
            vrf_materials: None,
        }
    }

    pub fn with_bootstrap_seed_and_vrf_materials(
        seed: Vec<u8>,
        vrf_materials: VrfMaterialProvider<V>,
    ) -> Self {
        Self {
            bootstrap_seed: Some(seed),
            vrf_materials: Some(vrf_materials),
        }
    }
}

impl<V: Variant> elector::Config<HybridScheme<V>> for HybridRandom<V> {
    type Elector = HybridRandomElector<V>;

    fn build(self, participants: &Set<bls12381::PublicKey>) -> HybridRandomElector<V> {
        assert!(!participants.is_empty(), "no participants");
        HybridRandomElector {
            n: participants.len() as u32,
            bootstrap_seed: self.bootstrap_seed,
            vrf_materials: self.vrf_materials,
            _phantom: std::marker::PhantomData,
        }
    }
}

/// Initialized hybrid random elector.
#[derive(Clone, Debug)]
pub struct HybridRandomElector<V: Variant> {
    n: u32,
    bootstrap_seed: Option<Vec<u8>>,
    vrf_materials: Option<VrfMaterialProvider<V>>,
    _phantom: std::marker::PhantomData<V>,
}

impl<V: Variant> elector::Elector<HybridScheme<V>> for HybridRandomElector<V> {
    fn elect(&self, round: Round, certificate: Option<&HybridCertificate<V>>) -> Participant {
        let verified_seed = match (certificate, &self.vrf_materials) {
            (Some(certificate), Some(provider)) => {
                let proof = certificate.vrf_proof.as_ref();
                proof.and_then(|proof| {
                    let seed_round = round
                        .view()
                        .previous()
                        .map(|view| Round::new(round.epoch(), view))?;
                    let namespace = crate::config::simplex_namespace();
                    let seed_message = seed_round.encode();
                    let mut rng = bls_batch_verification_rng();
                    provider
                        .verify_proof(
                            &mut rng,
                            proof,
                            &namespace.seed,
                            seed_message.as_ref(),
                            &commonware_parallel::Sequential,
                        )
                        .then(|| proof.threshold_signature.encode().to_vec())
                })
            }
            _ => None,
        };

        let seed_bytes = verified_seed
            .or_else(|| {
                (round.view() == commonware_consensus::types::View::new(1))
                    .then(|| self.bootstrap_seed.clone())
                    .flatten()
            })
            .or_else(|| self.degraded_seed(round, certificate));

        let Some(seed_bytes) = seed_bytes else {
            let leader = Participant::new(
                (round.epoch().get().wrapping_add(round.view().get())) as u32 % self.n,
            );
            tracing::debug!(
                epoch = round.epoch().get(),
                view = round.view().get(),
                leader = leader.get(),
                "leader elected via round-robin (no usable VRF seed)"
            );
            return leader;
        };

        let leader = Participant::new(modulo(seed_bytes.as_ref(), self.n as u64) as u32);
        tracing::debug!(
            epoch = round.epoch().get(),
            view = round.view().get(),
            leader = leader.get(),
            has_certificate = certificate.is_some(),
            has_bootstrap_seed = self.bootstrap_seed.is_some(),
            "leader elected via verified/degraded VRF seed"
        );
        leader
    }
}

impl<V: Variant> HybridRandomElector<V> {
    fn degraded_seed(
        &self,
        round: Round,
        certificate: Option<&HybridCertificate<V>>,
    ) -> Option<Vec<u8>> {
        let _certificate = certificate?;
        crate::metrics::record_vrf_degraded_leader_selection();
        let mut seed = self.bootstrap_seed.clone().unwrap_or_default();
        if seed.is_empty() {
            return None;
        }
        seed.extend_from_slice(&round.encode());
        tracing::warn!(
            epoch = round.epoch().get(),
            view = round.view().get(),
            "verified VRF proof missing or invalid; using deterministic degraded leader seed"
        );
        Some(seed)
    }
}

/// Epoch-scoped provider of leader elector configs.
///
/// The config includes the epoch bootstrap seed used by the Simplex elector.
/// Metadata validation uses this to recompute missed-proposer attribution with
/// the same deterministic inputs as the reporter path.
#[derive(Clone, Debug)]
pub struct HybridElectorConfigProvider<V: Variant> {
    inner: crate::epoch_registry::EpochRegistry<HybridRandom<V>>,
}

impl<V: Variant> HybridElectorConfigProvider<V> {
    /// Create an empty provider.
    pub fn new() -> Self {
        Self {
            inner: crate::epoch_registry::EpochRegistry::new(),
        }
    }

    /// Register the elector config for the given epoch (insert-once).
    pub fn register(&self, epoch: Epoch, config: HybridRandom<V>) -> bool {
        self.inner.register(epoch, config)
    }

    /// Remove the config for the given epoch.
    pub fn remove(&self, epoch: &Epoch) -> bool {
        self.inner.remove(epoch)
    }

    /// Return the registered config for an epoch.
    pub fn scoped(&self, epoch: Epoch) -> Option<Arc<HybridRandom<V>>> {
        self.inner.get(&epoch)
    }
}

impl<V: Variant> Default for HybridElectorConfigProvider<V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bls::bootstrap_dkg;
    use crate::hybrid::test_support::{test_participants, TestScheme, NAMESPACE};
    use commonware_consensus::simplex::elector::{Config as _, Elector as _};
    use commonware_consensus::{simplex::types::Subject, types::View};
    use commonware_cryptography::{certificate::Scheme as _, sha256::Digest as Sha256Digest};
    use commonware_parallel::Sequential;
    use commonware_utils::{ordered::Quorum as _, N3f1};

    #[test]
    fn test_hybrid_elector() {
        let (_, participants) = test_participants(3);

        let elector: HybridRandomElector<MinSig> = HybridRandom::default().build(&participants);

        // View 1 should fall back to round-robin (no certificate)
        let round = Round::new(Epoch::new(0), View::new(1));
        let leader = elector.elect(round, None);
        assert!(leader.get() < 3);
    }

    #[test]
    fn test_hybrid_elector_epoch_view_one_uses_bootstrap_seed() {
        let (keys, participants) = test_participants(3);
        let dkg = bootstrap_dkg(3).unwrap();

        let schemes: Vec<TestScheme> = keys
            .iter()
            .map(|key| {
                let pk = bls12381::PublicKey::from(key.clone());
                let idx = participants.index(&pk).unwrap();
                HybridScheme::signer(
                    NAMESPACE,
                    participants.clone(),
                    key.clone(),
                    dkg.polynomial.clone(),
                    dkg.shares[idx.get() as usize].clone(),
                )
                .unwrap()
            })
            .collect();

        let epoch = Epoch::new(0);
        let view = View::new(2);
        let subject = Subject::Nullify {
            round: Round::new(epoch, view),
        };

        let attestations: Vec<_> = schemes
            .iter()
            .map(|s| s.sign::<Sha256Digest>(subject).unwrap())
            .collect();

        let certificate = schemes[0]
            .assemble::<_, N3f1>(attestations, &Sequential)
            .unwrap();
        let seed = certificate.raw_vrf_seed_bytes().unwrap();

        let elector: HybridRandomElector<MinSig> =
            HybridRandom::with_bootstrap_seed(seed.clone()).build(&participants);

        let leader = elector.elect(Round::new(Epoch::new(1), View::new(1)), None);
        let expected = Participant::new(modulo(seed.as_ref(), participants.len() as u64) as u32);

        assert_eq!(leader, expected);
    }

    #[test]
    fn test_hybrid_elector_with_certificate() {
        let (keys, participants) = test_participants(3);
        let dkg = bootstrap_dkg(3).unwrap();

        let schemes: Vec<TestScheme> = keys
            .iter()
            .map(|key| {
                let pk = bls12381::PublicKey::from(key.clone());
                let idx = participants.index(&pk).unwrap();
                HybridScheme::signer(
                    NAMESPACE,
                    participants.clone(),
                    key.clone(),
                    dkg.polynomial.clone(),
                    dkg.shares[idx.get() as usize].clone(),
                )
                .unwrap()
            })
            .collect();

        let epoch = Epoch::new(1);
        let view = View::new(2);
        let subject = Subject::Nullify {
            round: Round::new(epoch, view),
        };

        let attestations: Vec<_> = schemes
            .iter()
            .map(|s| s.sign::<Sha256Digest>(subject).unwrap())
            .collect();

        let certificate = schemes[0]
            .assemble::<_, N3f1>(attestations, &Sequential)
            .unwrap();

        let elector: HybridRandomElector<MinSig> = HybridRandom::default().build(&participants);

        // With certificate, should get deterministic leader
        let round = Round::new(epoch, View::new(3));
        let leader1 = elector.elect(round, Some(&certificate));
        let leader2 = elector.elect(round, Some(&certificate));
        assert_eq!(leader1, leader2, "same certificate should give same leader");
    }
}
