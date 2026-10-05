//! Shared test data construction, independent of production proof validation.

use alloy_consensus::{Header, Sealable};
use alloy_primitives::{Address, B256};
use commonware_codec::Encode;
use commonware_consensus::{
    simplex::types::{Proposal, Subject},
    types::{Epoch, Round, View},
};
use commonware_cryptography::{
    bls12381::{
        self,
        dkg::feldman_desmedt::Output,
        primitives::{
            group::Share,
            sharing::Sharing,
            variant::{MinSig, Variant},
        },
    },
    certificate::Scheme,
    Hasher as _, Sha256, Signer as _,
};
use commonware_parallel::Sequential;
use commonware_utils::{
    ordered::{Quorum as _, Set},
    TryCollect as _,
};
use outbe_primitives::{
    consensus::DkgBoundaryArtifact,
    reshare_artifact::{
        encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, OutbeBlockArtifacts,
    },
    OutbeHeader,
};

use crate::{
    bls::bootstrap_dkg,
    digest::Digest,
    hybrid::HybridScheme,
    proof::{CommitteeEntry, CommitteeSnapshot},
    validators::ValidatorSet,
};

pub fn committee_snapshot(
    public_keys: &[bls12381::PublicKey],
    group_public_key: &<MinSig as Variant>::Public,
    vrf_material_version: u64,
) -> CommitteeSnapshot {
    let committee = committee_entries(public_keys.iter());
    CommitteeSnapshot {
        committee,
        vrf_material_version,
        vrf_group_public_key_bytes: group_public_key.encode().to_vec(),
        vrf_public_polynomial_hash: B256::ZERO,
    }
}

pub fn committee_entries<'a>(
    public_keys: impl Iterator<Item = &'a bls12381::PublicKey>,
) -> Vec<CommitteeEntry> {
    public_keys
        .enumerate()
        .map(|(i, pk)| {
            let mut consensus_pubkey = [0u8; 48];
            consensus_pubkey.copy_from_slice(pk.encode().as_ref());
            CommitteeEntry {
                address: Address::with_last_byte((i + 1) as u8),
                consensus_pubkey,
            }
        })
        .collect()
}

pub enum ResolverVote {
    Notarize,
    Finalize,
}

pub struct SignedResolverProposal {
    pub proposal: Proposal<Digest>,
    pub certificate:
        <HybridScheme<MinSig> as commonware_cryptography::certificate::Verifier>::Certificate,
    pub verifier: HybridScheme<MinSig>,
}

pub fn signed_resolver_proposal(
    round: Round,
    parent_view: View,
    payload: &[u8],
    vote: ResolverVote,
) -> SignedResolverProposal {
    let keys: Vec<bls12381::PrivateKey> = (0..3u8)
        .map(|i| bls12381::PrivateKey::from_seed((i + 1) as u64))
        .collect();
    let set: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|sk| bls12381::PublicKey::from(sk.clone()))
        .try_collect()
        .unwrap();
    let dkg = bootstrap_dkg(3).unwrap();
    let schemes: Vec<HybridScheme<MinSig>> = fixture_signer_schemes(
        b"resolver-test",
        &keys,
        &set,
        FixtureSignerSharing {
            polynomial: &dkg.polynomial,
            shares: &dkg.shares,
        },
    );
    let verifier = HybridScheme::<MinSig>::verifier(b"resolver-test", set, dkg.polynomial).unwrap();
    let digest = Digest::from(B256::from_slice(Sha256::hash(&[payload]).as_ref()));
    let proposal = Proposal::new(round, parent_view, digest);
    let subject = match vote {
        ResolverVote::Notarize => Subject::Notarize {
            proposal: &proposal,
        },
        ResolverVote::Finalize => Subject::Finalize {
            proposal: &proposal,
        },
    };
    let attestations: Vec<_> = schemes
        .iter()
        .map(|s| s.sign::<Digest>(subject).unwrap())
        .collect();
    let certificate = verifier
        .assemble(
            commonware_utils::iter::NonEmpty::try_new(attestations.into_iter()).unwrap(),
            &Sequential,
        )
        .unwrap();
    SignedResolverProposal {
        proposal,
        certificate,
        verifier,
    }
}

pub struct FixtureSignerSharing<'a> {
    pub polynomial: &'a Sharing<MinSig>,
    pub shares: &'a [Share],
}

pub fn fixture_signer_schemes(
    namespace: &[u8],
    keys: &[bls12381::PrivateKey],
    participants: &Set<bls12381::PublicKey>,
    sharing: FixtureSignerSharing<'_>,
) -> Vec<HybridScheme<MinSig>> {
    let FixtureSignerSharing { polynomial, shares } = sharing;
    keys.iter()
        .map(|key| {
            let pk = bls12381::PublicKey::from(key.clone());
            let idx = participants.index(&pk).unwrap();
            HybridScheme::signer(
                namespace,
                participants.clone(),
                key.clone(),
                polynomial.clone(),
                shares[idx.get() as usize].clone(),
            )
            .unwrap()
        })
        .collect()
}

pub struct BoundaryFixtureSettings {
    pub is_full_dkg: bool,
    pub freeze_height: u64,
    pub planned_activation_height: u64,
    pub is_validator_set_change: bool,
}

pub fn boundary_artifact(
    epoch: u64,
    validator_set: &ValidatorSet,
    output: &Output<MinSig, bls12381::PublicKey>,
    settings: BoundaryFixtureSettings,
) -> DkgBoundaryArtifact {
    crate::dkg_manager::build_boundary_artifact(crate::dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(epoch),
        validator_set,
        output,
        is_full_dkg: settings.is_full_dkg,
        dkg_cycle: epoch,
        freeze_height: settings.freeze_height,
        planned_activation_height: settings.planned_activation_height,
        vrf_material_version: epoch,
        is_validator_set_change: settings.is_validator_set_change,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap()
}

pub fn linked_headers(
    genesis: OutbeHeader,
    last_height: u64,
    commitment_scheme_version: u32,
    mut root: impl FnMut() -> B256,
) -> Vec<OutbeHeader> {
    let mut headers = vec![genesis];
    for height in 1..=last_height {
        headers.push(OutbeHeader::new(Header {
            number: height,
            parent_hash: headers.last().unwrap().hash_slow(),
            extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                    commitment_scheme_version,
                    r_sealed: root(),
                }),
                ..Default::default()
            })
            .unwrap(),
            ..Default::default()
        }));
    }
    headers
}
