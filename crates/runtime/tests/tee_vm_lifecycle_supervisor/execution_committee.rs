//! A committee for certifying headers produced by execution rather than synthetic headers.
use commonware_codec::Encode as _;
use commonware_consensus::{
    simplex::types::{Proposal, Subject},
    types::{Epoch, Round, View},
};
use commonware_cryptography::{
    bls12381::{self, primitives::variant::MinSig},
    certificate::Scheme as _,
    Signer as _,
};
use commonware_parallel::Sequential;
use commonware_utils::{
    iter::NonEmpty,
    ordered::{Quorum as _, Set},
    TryCollect as _,
};
use outbe_consensus::{
    bls::{bootstrap_dkg_for_participants, odko, ParticipantDkgBootstrapResult},
    digest::Digest,
    hybrid::{HybridScheme, VrfMaterialProvider},
    marshal_types::Finalization,
};

pub(super) struct Committee {
    participants: Set<bls12381::PublicKey>,
    keys: Vec<bls12381::PrivateKey>,
    dkg: ParticipantDkgBootstrapResult,
}

impl Committee {
    pub(super) fn new(seed: u64) -> Self {
        let keys: Vec<_> = (seed + 1..=seed + 4)
            .map(bls12381::PrivateKey::from_seed)
            .collect();
        let participants: Set<_> = keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();
        let dkg = bootstrap_dkg_for_participants(participants.clone()).unwrap();
        Self {
            participants,
            keys,
            dkg,
        }
    }

    #[allow(dead_code)]
    pub(super) fn snapshot(
        &self,
        addresses: [alloy_primitives::Address; 4],
    ) -> outbe_validatorset::CommitteeSnapshot {
        let committee = self
            .participants
            .iter()
            .zip(addresses)
            .map(|(key, address)| outbe_validatorset::CommitteeEntry {
                address,
                consensus_pubkey: key.encode().as_ref().try_into().unwrap(),
            })
            .collect();
        outbe_validatorset::CommitteeSnapshot {
            committee,
            vrf_material_version: 0,
            vrf_group_public_key_bytes: self.dkg.polynomial.public().encode().to_vec(),
            vrf_public_polynomial_hash: alloy_primitives::keccak256(self.dkg.polynomial.encode()),
        }
    }

    #[allow(dead_code)]
    pub(super) fn outcome(&self) -> Vec<u8> {
        odko::encode_outcome(Epoch::new(0), &self.dkg.output, false).to_vec()
    }

    pub(super) fn certify(&self, digest: Digest, height: u64) -> Vec<u8> {
        let epoch = Epoch::new(0);
        let proposal = Proposal::new(
            Round::new(epoch, View::new(height + 1)),
            View::new(height),
            digest,
        );
        let subject = Subject::Finalize {
            proposal: &proposal,
        };
        let namespace = outbe_consensus::config::outbe_app_namespace();
        let material = |share| VrfMaterialProvider::new(0, self.dkg.polynomial.clone(), share);
        let verifier = HybridScheme::<MinSig>::verifier_with_vrf_provider(
            &namespace,
            self.participants.clone(),
            material(None),
        )
        .unwrap();
        let signatures: Vec<_> = self
            .keys
            .iter()
            .map(|key| {
                let index = self.participants.index(&key.public_key()).unwrap();
                let share = self.dkg.shares[index.get() as usize].clone();
                HybridScheme::<MinSig>::signer_with_vrf_provider(
                    &namespace,
                    self.participants.clone(),
                    key.clone(),
                    material(Some(share)),
                )
                .unwrap()
                .sign::<Digest>(subject)
                .unwrap()
            })
            .collect();
        let certificate = verifier
            .assemble(
                NonEmpty::try_new(signatures.into_iter()).unwrap(),
                &Sequential,
            )
            .unwrap();
        Finalization {
            proposal,
            certificate,
        }
        .encode()
        .to_vec()
    }
}
