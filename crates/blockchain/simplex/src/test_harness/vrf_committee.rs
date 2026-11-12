//! A deterministic BLS committee with a VRF key pair for proof fixtures.

use alloy_primitives::B256;
use commonware_codec::Encode;
use commonware_consensus::{
    simplex::types::Proposal,
    types::{Epoch, Round, View},
};
use commonware_cryptography::{
    bls12381::{
        primitives::{
            group::Private,
            ops::{aggregate, keypair, sign_message},
            variant::{MinPk, MinSig, Variant},
        },
        PrivateKey, PublicKey,
    },
    certificate::Signers,
    sha256::Digest as Sha256Digest,
    Signer as _,
};
use commonware_utils::{iter::NonEmpty, ordered::Set, Participant};
use rand_commonware::{rngs::ChaCha20Rng, SeedableRng};

use super::fixtures::committee_snapshot;
use crate::proof::{hybrid_seed_namespace, CommitteeSnapshot, HybridCertificate, VrfProof};

/// Consensus keys and the VRF threshold key pair of a test committee.
pub struct VrfTestCommittee {
    pub keys: Vec<PrivateKey>,
    pub pubkeys: Vec<PublicKey>,
    pub vrf_group_public_key: <MinSig as Variant>::Public,
    pub vrf_threshold_private: Private,
}

/// The messages that a test certificate signs.
pub struct CertificateMessages<'a> {
    pub vote_namespace: &'a [u8],
    pub vote: &'a [u8],
    pub seed: &'a [u8],
}

/// `n` consensus keys from the seeds 1 to `n`, in that order, and a VRF key
/// pair from the ChaCha20 seed 13.
pub fn vrf_test_committee(n: u32) -> VrfTestCommittee {
    let keys: Vec<PrivateKey> = (0..n)
        .map(|i| PrivateKey::from_seed(i as u64 + 1))
        .collect();
    let pubkeys: Vec<PublicKey> = keys.iter().cloned().map(PublicKey::from).collect();
    let mut rng = ChaCha20Rng::seed_from_u64(13);
    let (vrf_threshold_private, vrf_group_public_key) = keypair::<_, MinSig>(&mut rng);
    VrfTestCommittee {
        keys,
        pubkeys,
        vrf_group_public_key,
        vrf_threshold_private,
    }
}

/// The round, the vote message and the seed message of the proposal at
/// `epoch` and `view` over `parent_hash`, with `parent_view` as its parent.
pub fn finalize_messages(
    epoch: u64,
    view: u64,
    parent_view: u64,
    parent_hash: B256,
) -> (Round, Vec<u8>, Vec<u8>) {
    let round = Round::new(Epoch::new(epoch), View::new(view));
    let payload = Sha256Digest(parent_hash.0);
    let proposal: Proposal<Sha256Digest> = Proposal::new(round, View::new(parent_view), payload);
    let vote_message = proposal.encode().to_vec();
    let seed_message = round.encode().to_vec();
    (round, vote_message, seed_message)
}

impl VrfTestCommittee {
    /// The committee snapshot of these keys for VRF material
    /// `vrf_material_version`.
    pub fn snapshot(&self, vrf_material_version: u64) -> CommitteeSnapshot {
        committee_snapshot(
            &self.pubkeys,
            &self.vrf_group_public_key,
            vrf_material_version,
        )
    }

    /// The ordered committee set that the vote namespaces bind.
    pub fn committee_set(&self) -> Set<PublicKey> {
        Set::from_iter_dedup(self.pubkeys.iter().cloned())
    }

    /// The certificate in which `signer_indices` sign the vote message and
    /// `vrf_signer` signs the seed message for VRF material `material_version`.
    pub fn certificate(
        &self,
        signer_indices: &[u32],
        messages: &CertificateMessages<'_>,
        vrf_signer: &Private,
        material_version: u64,
    ) -> HybridCertificate<MinSig> {
        let signers = Signers::new(
            self.keys.len() as u32,
            signer_indices.iter().copied().map(Participant::new),
        )
        .unwrap();
        let sigs: Vec<_> = signer_indices
            .iter()
            .map(|&i| self.keys[i as usize].sign(messages.vote_namespace, messages.vote))
            .collect();
        let bls_aggregated_vote = aggregate::combine_signatures::<MinPk, _>(
            NonEmpty::try_new(sigs.iter().map(|s| s.as_ref())).unwrap(),
        );
        let threshold_signature =
            sign_message::<MinSig>(vrf_signer, &hybrid_seed_namespace(), messages.seed);
        HybridCertificate {
            signers,
            bls_aggregated_vote,
            vrf_proof: VrfProof::<MinSig> {
                material_version,
                threshold_signature,
            },
        }
    }
}
