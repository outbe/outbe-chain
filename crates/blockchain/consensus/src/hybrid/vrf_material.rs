//! Versioned threshold (VRF) material for the hybrid scheme.
//!
//! [`VrfMaterialProvider`] owns the per-version DKG material (polynomial +
//! optional local share) and every threshold-crypto operation over it: seed
//! signing, proof recovery, and partial/proof verification. The provider fully
//! encapsulates the state. The surrounding `HybridScheme` holds a provider and
//! calls its methods, and never reaches into the version map. This module was
//! extracted from `hybrid.rs` so that the material lifecycle reads and tests as
//! one unit.
//! The dependency is one-way (`HybridScheme` -> provider).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use commonware_cryptography::bls12381::primitives::{
    group::Share,
    ops::{self, threshold},
    sharing::Sharing,
    variant::{MinSig, PartialSignature, Variant},
};
use commonware_parallel::Strategy;
use commonware_utils::Participant;

use crate::proof::hybrid_wire::VrfProof;

/// Inputs for verifying a single VRF seed partial against a committee
/// polynomial at a given material version.
pub(crate) struct VrfPartialVerification<'a, V: Variant> {
    pub(crate) version: u64,
    pub(crate) signer: Participant,
    pub(crate) namespace: &'a [u8],
    pub(crate) seed_message: &'a [u8],
    pub(crate) signature: V::Signature,
}

#[derive(Clone, Debug)]
struct VrfMaterial<V: Variant> {
    polynomial: Sharing<V>,
    share: Option<Share>,
}

/// Shared, versioned threshold material used by the VRF sidecar path.
#[derive(Clone, Debug)]
pub struct VrfMaterialProvider<V: Variant> {
    inner: Arc<Mutex<VrfMaterialState<V>>>,
}

#[derive(Clone, Debug)]
struct VrfMaterialState<V: Variant> {
    active_version: u64,
    materials: HashMap<u64, VrfMaterial<V>>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RepositorySigningError {
    #[error("repository signing material is not current")]
    InactiveMaterial,
    #[error("repository group key differs from the authenticated authority")]
    GroupKeyMismatch,
    #[error("repository polynomial differs from the authenticated current material")]
    PolynomialMismatch,
    #[error("local repository signing share is unavailable")]
    MissingShare,
    #[error("local share index differs from the authenticated participant")]
    ParticipantMismatch,
    #[error("local repository signing share does not match the public polynomial")]
    InvalidShare,
    #[error("repository partial is invalid for the current subject and material")]
    InvalidPartial,
    #[error("duplicate repository participant index")]
    DuplicatePartial,
    #[error("not enough distinct repository partials")]
    InsufficientPartials,
}

impl VrfMaterialProvider<MinSig> {
    fn current_repository_material<'a>(
        state: &'a VrfMaterialState<MinSig>,
        version: u64,
        polynomial: &Sharing<MinSig>,
    ) -> Result<&'a VrfMaterial<MinSig>, RepositorySigningError> {
        if state.active_version != version {
            return Err(RepositorySigningError::InactiveMaterial);
        }
        let material = state
            .materials
            .get(&version)
            .ok_or(RepositorySigningError::InactiveMaterial)?;
        if material.polynomial.public() != polynomial.public() {
            return Err(RepositorySigningError::GroupKeyMismatch);
        }
        if material.polynomial != *polynomial {
            return Err(RepositorySigningError::PolynomialMismatch);
        }
        Ok(material)
    }

    /// Verify a contribution against the still-current authenticated polynomial before storing it.
    pub fn verify_repository_partial(
        &self,
        version: u64,
        polynomial: &Sharing<MinSig>,
        subject: &radicle_crypto::RepositorySubject,
        partial: &PartialSignature<MinSig>,
    ) -> Result<(), RepositorySigningError> {
        self.with_state(|state| {
            let material = Self::current_repository_material(state, version, polynomial)?;
            threshold::verify_message::<MinSig>(
                &material.polynomial,
                radicle_crypto::repository::DOMAIN,
                subject.as_bytes(),
                partial,
            )
            .map_err(|_| RepositorySigningError::InvalidPartial)
        })
    }

    /// Verify each indexed partial before interpolation under the current polynomial.
    pub fn recover_repository(
        &self,
        version: u64,
        polynomial: &Sharing<MinSig>,
        subject: &radicle_crypto::RepositorySubject,
        partials: &[PartialSignature<MinSig>],
    ) -> Result<<MinSig as Variant>::Signature, RepositorySigningError> {
        self.with_state(|state| {
            let material = Self::current_repository_material(state, version, polynomial)?;
            let mut participants = std::collections::BTreeSet::new();
            for partial in partials {
                if !participants.insert(partial.index) {
                    return Err(RepositorySigningError::DuplicatePartial);
                }
                threshold::verify_message::<MinSig>(
                    &material.polynomial,
                    radicle_crypto::repository::DOMAIN,
                    subject.as_bytes(),
                    partial,
                )
                .map_err(|_| RepositorySigningError::InvalidPartial)?;
            }
            let recovered = threshold::recover::<MinSig, _>(
                &material.polynomial,
                partials.iter(),
                &commonware_parallel::Sequential,
            )
            .map_err(|_| RepositorySigningError::InsufficientPartials)?;
            ops::verify_message::<MinSig>(
                polynomial.public(),
                radicle_crypto::repository::DOMAIN,
                subject.as_bytes(),
                &recovered,
            )
            .map_err(|_| RepositorySigningError::InvalidPartial)?;
            Ok(recovered)
        })
    }

    /// Sign a native repository subject under current material only. The caller authenticates
    /// the finalized job and participant mapping before requesting this fixed-domain operation.
    pub fn sign_repository(
        &self,
        version: u64,
        participant: Participant,
        polynomial: &Sharing<MinSig>,
        subject: &radicle_crypto::RepositorySubject,
    ) -> Result<PartialSignature<MinSig>, RepositorySigningError> {
        self.with_state(|state| {
            let material = Self::current_repository_material(state, version, polynomial)?;
            let share = material
                .share
                .as_ref()
                .ok_or(RepositorySigningError::MissingShare)?;
            if share.index != participant {
                return Err(RepositorySigningError::ParticipantMismatch);
            }
            let partial = threshold::sign_message::<MinSig>(
                share,
                radicle_crypto::repository::DOMAIN,
                subject.as_bytes(),
            );
            threshold::verify_message::<MinSig>(
                &material.polynomial,
                radicle_crypto::repository::DOMAIN,
                subject.as_bytes(),
                &partial,
            )
            .map_err(|_| RepositorySigningError::InvalidShare)?;
            Ok(partial)
        })
    }
}

impl<V: Variant> VrfMaterialProvider<V> {
    pub fn new(active_version: u64, polynomial: Sharing<V>, share: Option<Share>) -> Self {
        polynomial.precompute_partial_publics();
        let mut materials = HashMap::new();
        materials.insert(active_version, VrfMaterial { polynomial, share });
        Self {
            inner: Arc::new(Mutex::new(VrfMaterialState {
                active_version,
                materials,
            })),
        }
    }

    pub fn active_version(&self) -> u64 {
        self.with_state(|state| state.active_version)
    }

    pub fn active_polynomial_total(&self) -> Option<u32> {
        self.polynomial_total(self.active_version())
    }

    pub(crate) fn polynomial_total(&self, version: u64) -> Option<u32> {
        self.with_state(|state| {
            state
                .materials
                .get(&version)
                .map(|material| material.polynomial.total().get())
        })
    }

    pub(crate) fn share(&self, version: u64) -> Option<Share> {
        self.with_state(|state| {
            state
                .materials
                .get(&version)
                .and_then(|material| material.share.clone())
        })
    }

    pub(crate) fn public(&self, version: u64) -> Option<V::Public> {
        self.with_state(|state| {
            state
                .materials
                .get(&version)
                .map(|material| *material.polynomial.public())
        })
    }

    /// Partial public key for `index` in the polynomial of material `version`,
    /// or `None` if there is no material for `version` or the index is out of
    /// range.
    /// Lets callers validate a local share without exposing the state map.
    pub(crate) fn partial_public(&self, version: u64, index: Participant) -> Option<V::Public> {
        self.with_state(|state| {
            state
                .materials
                .get(&version)
                .and_then(|material| material.polynomial.partial_public(index).ok())
        })
    }

    pub fn activate(&self, version: u64, polynomial: Sharing<V>, share: Option<Share>) {
        polynomial.precompute_partial_publics();
        self.with_state(|state| {
            state
                .materials
                .insert(version, VrfMaterial { polynomial, share });
            state.active_version = version;
        });
    }

    pub(crate) fn sign_seed(
        &self,
        version: u64,
        namespace: &[u8],
        seed_message: &[u8],
    ) -> Option<V::Signature> {
        self.with_state(|state| {
            let material = state.materials.get(&version)?;
            let share = material.share.as_ref()?;
            let partial = threshold::sign_message::<V>(share, namespace, seed_message).value;
            Some(partial)
        })
    }

    pub(crate) fn recover_proof(
        &self,
        version: u64,
        seed_partials: &[PartialSignature<V>],
        strategy: &impl Strategy,
    ) -> Option<VrfProof<V>> {
        self.with_state(|state| {
            let material = state.materials.get(&version)?;
            let signature =
                threshold::recover::<V, _>(&material.polynomial, seed_partials.iter(), strategy)
                    .ok()?;
            Some(VrfProof {
                material_version: version,
                threshold_signature: signature,
            })
        })
    }

    pub(crate) fn verify_partial(&self, input: VrfPartialVerification<'_, V>) -> bool {
        let VrfPartialVerification {
            version,
            signer,
            namespace,
            seed_message,
            signature,
        } = input;

        self.with_state(|state| {
            let Some(material) = state.materials.get(&version) else {
                return false;
            };
            threshold::verify_message::<V>(
                &material.polynomial,
                namespace,
                seed_message,
                &PartialSignature {
                    index: signer,
                    value: signature,
                },
            )
            .is_ok()
        })
    }

    pub(crate) fn verify_proof(
        &self,
        proof: &VrfProof<V>,
        namespace: &[u8],
        seed_message: &[u8],
    ) -> bool {
        self.with_state(|state| {
            let Some(material) = state.materials.get(&proof.material_version) else {
                return false;
            };
            ops::verify_message::<V>(
                material.polynomial.public(),
                namespace,
                seed_message,
                &proof.threshold_signature,
            )
            .is_ok()
        })
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut VrfMaterialState<V>) -> T) -> T {
        let mut state = self.inner.lock().unwrap_or_else(|poisoned| {
            tracing::error!("VrfMaterialProvider mutex poisoned, recovering");
            poisoned.into_inner()
        });
        f(&mut state)
    }
}

#[cfg(test)]
mod repository_tests {
    use super::*;
    use crate::bls::bootstrap_dkg;
    use commonware_codec::Encode as _;
    use radicle_crypto::{BlsEnvelope, BlsPublicKey, RepositorySubject};

    fn subject() -> RepositorySubject {
        RepositorySubject::cob([1; 20], [3; 32], [4; 20], b"native Review Accept bytes")
    }

    #[test]
    fn repository_threshold_partials_recover_native_bls_and_are_isolated_from_vrf() {
        let dkg = bootstrap_dkg(4).unwrap();
        assert_eq!(dkg.polynomial.required(), 3);
        let group = *dkg.polynomial.public();
        let message = subject();
        let providers: Vec<_> = dkg
            .shares
            .iter()
            .map(|share| VrfMaterialProvider::new(7, dkg.polynomial.clone(), Some(share.clone())))
            .collect();
        let partials: Vec<_> = providers
            .iter()
            .enumerate()
            .map(|(index, provider)| {
                let partial = provider
                    .sign_repository(7, Participant::from_usize(index), &dkg.polynomial, &message)
                    .unwrap();
                threshold::verify_message::<MinSig>(
                    &dkg.polynomial,
                    radicle_crypto::repository::DOMAIN,
                    message.as_bytes(),
                    &partial,
                )
                .unwrap();
                partial
            })
            .collect();
        let first = providers[0]
            .recover_repository(7, &dkg.polynomial, &message, &partials[..3])
            .unwrap();
        let second = providers[0]
            .recover_repository(7, &dkg.polynomial, &message, &partials[1..])
            .unwrap();
        assert_eq!(first, second);
        let owner = BlsPublicKey::from_bytes(group.encode().to_vec().try_into().unwrap()).unwrap();
        let envelope = BlsEnvelope::attach(
            message.clone(),
            owner,
            first.encode().to_vec().try_into().unwrap(),
        )
        .unwrap();
        envelope.verify(&message, owner.into()).unwrap();
        assert!(matches!(
            providers[0].recover_repository(7, &dkg.polynomial, &message, &partials[..2]),
            Err(RepositorySigningError::InsufficientPartials)
        ));
        assert!(matches!(
            providers[0].recover_repository(
                7,
                &dkg.polynomial,
                &message,
                &[
                    partials[0].clone(),
                    partials[0].clone(),
                    partials[1].clone()
                ]
            ),
            Err(RepositorySigningError::DuplicatePartial)
        ));
        let mut wrong_index = partials[..3].to_vec();
        wrong_index[0].index = Participant::new(4);
        assert!(matches!(
            providers[0].recover_repository(7, &dkg.polynomial, &message, &wrong_index),
            Err(RepositorySigningError::InvalidPartial)
        ));

        for changed in [
            RepositorySubject::cob([2; 20], [3; 32], [4; 20], b"native Review Accept bytes"),
            RepositorySubject::cob([1; 20], [5; 32], [4; 20], b"native Review Accept bytes"),
            RepositorySubject::cob([1; 20], [3; 32], [4; 20], b"another native operation"),
        ] {
            assert!(matches!(
                providers[0].recover_repository(7, &dkg.polynomial, &changed, &partials[..3]),
                Err(RepositorySigningError::InvalidPartial)
            ));
            assert!(envelope.verify(&changed, owner.into()).is_err());
            assert!(threshold::verify_message::<MinSig>(
                &dkg.polynomial,
                radicle_crypto::repository::DOMAIN,
                changed.as_bytes(),
                &partials[0]
            )
            .is_err());
        }
        let seed_namespace = crate::proof::hybrid_seed_namespace();
        assert!(
            ops::verify_message::<MinSig>(&group, &seed_namespace, message.as_bytes(), &first)
                .is_err()
        );
        let seed = providers[0]
            .sign_seed(7, &seed_namespace, message.as_bytes())
            .unwrap();
        let seed_partial = PartialSignature::<MinSig> {
            index: Participant::new(0),
            value: seed,
        };
        assert!(threshold::verify_message::<MinSig>(
            &dkg.polynomial,
            radicle_crypto::repository::DOMAIN,
            message.as_bytes(),
            &seed_partial
        )
        .is_err());
    }

    #[test]
    fn repository_signing_rejects_old_material_wrong_participant_and_invalid_local_share() {
        let dkg = bootstrap_dkg(4).unwrap();
        let other = bootstrap_dkg(4).unwrap();
        let message = subject();
        let participant = dkg.shares[0].index;
        let provider =
            VrfMaterialProvider::new(7, dkg.polynomial.clone(), Some(dkg.shares[0].clone()));
        assert!(matches!(
            provider.sign_repository(6, participant, &dkg.polynomial, &message),
            Err(RepositorySigningError::InactiveMaterial)
        ));
        assert!(matches!(
            provider.sign_repository(7, Participant::new(4), &dkg.polynomial, &message),
            Err(RepositorySigningError::ParticipantMismatch)
        ));
        assert!(matches!(
            provider.sign_repository(7, participant, &other.polynomial, &message),
            Err(RepositorySigningError::GroupKeyMismatch)
        ));
        let verifier = VrfMaterialProvider::new(7, dkg.polynomial.clone(), None);
        assert!(matches!(
            verifier.sign_repository(7, participant, &dkg.polynomial, &message),
            Err(RepositorySigningError::MissingShare)
        ));
        let invalid =
            VrfMaterialProvider::new(7, dkg.polynomial.clone(), Some(other.shares[0].clone()));
        assert!(matches!(
            invalid.sign_repository(7, participant, &dkg.polynomial, &message),
            Err(RepositorySigningError::InvalidShare)
        ));

        let historical = provider
            .sign_repository(7, participant, &dkg.polynomial, &message)
            .unwrap();
        assert!(matches!(
            verifier.recover_repository(6, &dkg.polynomial, &message, &[]),
            Err(RepositorySigningError::InactiveMaterial)
        ));
        assert!(matches!(
            verifier.recover_repository(7, &other.polynomial, &message, &[]),
            Err(RepositorySigningError::GroupKeyMismatch)
        ));
        provider.activate(8, dkg.polynomial.clone(), Some(dkg.shares[0].clone()));
        assert!(matches!(
            provider.recover_repository(
                7,
                &dkg.polynomial,
                &message,
                std::slice::from_ref(&historical)
            ),
            Err(RepositorySigningError::InactiveMaterial)
        ));
        assert!(matches!(
            provider.sign_repository(7, participant, &dkg.polynomial, &message),
            Err(RepositorySigningError::InactiveMaterial)
        ));
        assert!(provider
            .sign_repository(8, participant, &dkg.polynomial, &message)
            .is_ok());
        threshold::verify_message::<MinSig>(
            &dkg.polynomial,
            radicle_crypto::repository::DOMAIN,
            message.as_bytes(),
            &historical,
        )
        .unwrap();
    }
}
