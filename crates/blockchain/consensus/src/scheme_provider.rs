//! Epoch-scoped scheme provider for BLS threshold VRF.
//!
//! Stores BLS12-381 threshold VRF schemes indexed by epoch, implementing the
//! `certificate::Provider` trait required by Simplex consensus.

use commonware_consensus::simplex::scheme::bls12381_threshold::vrf as bls_vrf;
use commonware_consensus::types::Epoch;
use commonware_cryptography::{
    bls12381::{self, primitives::variant::MinSig},
    certificate,
};
use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, Mutex},
};

/// BLS VRF scheme type used throughout the consensus layer.
pub type BlsVrfScheme = bls_vrf::Scheme<bls12381::PublicKey, MinSig>;

/// Epoch-scoped provider of BLS threshold VRF schemes.
///
/// Each epoch may have a different scheme instance (different polynomial/shares
/// after DKG ceremonies). The provider stores schemes indexed by epoch and
/// returns them on demand for signature verification.
#[derive(Clone, Debug)]
pub struct SchemeProvider {
    inner: Arc<Mutex<HashMap<Epoch, Arc<BlsVrfScheme>>>>,
}

impl SchemeProvider {
    /// Create an empty scheme provider.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a scheme for the given epoch.
    ///
    /// Returns `true` if this is a new registration, `false` if the epoch
    /// was already registered (the existing scheme is kept).
    pub fn register(&self, epoch: Epoch, scheme: BlsVrfScheme) -> bool {
        let mut schemes = self.inner.lock().unwrap_or_else(|poisoned| {
            tracing::error!("SchemeProvider mutex poisoned in register(), recovering");
            poisoned.into_inner()
        });
        match schemes.entry(epoch) {
            Entry::Vacant(entry) => {
                entry.insert(Arc::new(scheme));
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    /// Remove the scheme for the given epoch (to free memory after epoch ends).
    pub fn remove(&self, epoch: &Epoch) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::error!("SchemeProvider mutex poisoned in remove(), recovering");
                poisoned.into_inner()
            })
            .remove(epoch)
            .is_some()
    }
}

impl Default for SchemeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl certificate::Provider for SchemeProvider {
    type Scope = Epoch;
    type Scheme = BlsVrfScheme;

    fn scoped(&self, scope: Self::Scope) -> Option<Arc<Self::Scheme>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::error!("SchemeProvider mutex poisoned in scoped(), recovering");
                poisoned.into_inner()
            })
            .get(&scope)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bls::bootstrap_dkg;
    use commonware_cryptography::Signer as _;
    use commonware_utils::{ordered::Set, TryCollect as _};

    /// Generate `n` BLS MinPk identity keys and return them as an ordered Set.
    fn test_participants(n: u8) -> Set<bls12381::PublicKey> {
        (0..n)
            .map(|i| {
                let sk = bls12381::PrivateKey::from_seed((i + 1) as u64);
                bls12381::PublicKey::from(sk)
            })
            .try_collect()
            .unwrap()
    }

    #[test]
    fn test_scheme_provider_register_and_lookup() {
        let provider = SchemeProvider::new();
        let dkg = bootstrap_dkg(3).unwrap();
        let participants = test_participants(3);

        let scheme = BlsVrfScheme::verifier(
            &crate::config::outbe_app_namespace(),
            participants,
            dkg.polynomial.clone(),
        );

        let epoch = Epoch::new(1);
        assert!(provider.register(epoch, scheme));

        let dkg_replacement = bootstrap_dkg(3).unwrap();
        let participants_replacement = test_participants(3);
        let replacement = BlsVrfScheme::verifier(
            &crate::config::outbe_app_namespace(),
            participants_replacement,
            dkg_replacement.polynomial.clone(),
        );
        assert!(
            !provider.register(epoch, replacement),
            "duplicate epoch registration must not replace existing scheme"
        );

        // Should be retrievable
        assert!(certificate::Provider::scoped(&provider, epoch).is_some());

        // Non-existent epoch
        assert!(certificate::Provider::scoped(&provider, Epoch::new(2)).is_none());

        // Remove
        assert!(provider.remove(&epoch));
        assert!(certificate::Provider::scoped(&provider, epoch).is_none());
    }

    #[test]
    fn test_scheme_provider_signer() {
        let provider = SchemeProvider::new();
        let dkg = bootstrap_dkg(3).unwrap();
        let participants = test_participants(3);

        // Create a signer scheme for participant 0
        let scheme = BlsVrfScheme::signer(
            &crate::config::outbe_app_namespace(),
            participants,
            dkg.polynomial.clone(),
            dkg.shares[0].clone(),
        );
        assert!(
            scheme.is_some(),
            "signer scheme should be created successfully"
        );

        let epoch = Epoch::new(1);
        provider.register(epoch, scheme.unwrap());

        let retrieved = certificate::Provider::scoped(&provider, epoch);
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_scheme_provider_recovers_from_poisoned_mutex() {
        let provider = SchemeProvider::new();
        let dkg = bootstrap_dkg(3).unwrap();
        let participants = test_participants(3);

        let scheme = BlsVrfScheme::verifier(
            &crate::config::outbe_app_namespace(),
            participants,
            dkg.polynomial.clone(),
        );

        let epoch = Epoch::new(1);
        provider.register(epoch, scheme);

        // Poison the mutex by panicking while holding the lock.
        let provider_clone = provider.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = provider_clone.inner.lock().unwrap();
            panic!("intentional panic to poison mutex");
        }));
        assert!(result.is_err(), "should have panicked");

        // After poisoning, all operations should still work (no panic).
        // register
        let dkg2 = bootstrap_dkg(3).unwrap();
        let participants2 = test_participants(3);
        let scheme2 = BlsVrfScheme::verifier(
            &crate::config::outbe_app_namespace(),
            participants2,
            dkg2.polynomial.clone(),
        );
        let registered = provider.register(Epoch::new(2), scheme2);
        assert!(registered, "register should work after poison recovery");

        // scoped
        let found = certificate::Provider::scoped(&provider, Epoch::new(2));
        assert!(found.is_some(), "scoped should work after poison recovery");

        // remove
        let removed = provider.remove(&Epoch::new(2));
        assert!(removed, "remove should work after poison recovery");
    }
}
