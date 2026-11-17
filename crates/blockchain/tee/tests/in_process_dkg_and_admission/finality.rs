//! Local finality fixtures for outbe-tee, adapted from simplex test utilities.
//! Simplex's fixture is feature-gated and that feature is not enabled by this
//! package; this copy uses its public signing/bootstrap APIs instead.

use alloy_consensus::Block;
use alloy_primitives::{Bytes, B256};
use commonware_codec::Encode as _;
use commonware_consensus::simplex::types::{Proposal, Subject};
use commonware_consensus::types::{Epoch, Round, View};
use commonware_cryptography::{
    bls12381::{self, primitives::variant::MinSig},
    certificate::Scheme as _,
    Signer as _,
};
use commonware_parallel::Sequential;
use commonware_utils::{
    ordered::{Quorum as _, Set},
    TryCollect as _,
};
use outbe_primitives::{
    reshare_artifact::{
        encode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
    },
    OutbeHeader,
};
use reth_primitives_traits::SealedBlock;

use outbe_simplex::{
    block::ConsensusBlock,
    bls::ParticipantDkgBootstrapResult,
    digest::Digest,
    hybrid::{HybridScheme, VrfMaterialProvider},
    marshal_types::Finalization,
};

pub struct FinalityCommitteeFixture {
    keys: Vec<bls12381::PrivateKey>,
    participants: Set<bls12381::PublicKey>,
    dkg: ParticipantDkgBootstrapResult,
}

pub struct CertifiedBlockFixture {
    pub finalization: Vec<u8>,
    pub block: Vec<u8>,
    pub block_hash: B256,
}

/// Complete parent-bound block input for cryptographic finality fixtures.
pub struct CertifiedChildBlockInput {
    pub epoch: Epoch,
    pub height: u64,
    pub timestamp: u64,
    pub state_root: B256,
    pub extra_data: Vec<u8>,
    pub parent_hash: B256,
}

impl FinalityCommitteeFixture {
    pub fn new(seed_base: u64) -> Self {
        let mut keys = (1..=4)
            .map(|offset| bls12381::PrivateKey::from_seed(seed_base + offset))
            .collect::<Vec<_>>();
        keys.sort_by_key(|key| key.public_key().encode());
        let participants: Set<bls12381::PublicKey> = keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .expect("fixture keys are unique");
        let dkg = outbe_simplex::bls::bootstrap_dkg_for_participants(participants.clone())
            .expect("fixture DKG succeeds");
        Self {
            keys,
            participants,
            dkg,
        }
    }

    pub fn public_keys_min_pk(&self) -> Vec<[u8; 48]> {
        self.participants
            .iter()
            .map(|key| key.encode().as_ref().try_into().expect("MinPk is 48 bytes"))
            .collect()
    }

    pub fn outcome(&self, epoch: Epoch) -> Vec<u8> {
        outbe_simplex::bls::odko::encode_outcome(epoch, &self.dkg.output, false).to_vec()
    }

    pub fn preannounce_extra_data(&self, epoch: Epoch) -> Vec<u8> {
        encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            consensus_header_artifact: Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
                epoch: epoch.get(),
                outcome: Bytes::from(self.outcome(epoch)),
            }),
            ..Default::default()
        })
        .expect("fixture preannounce encodes")
        .to_vec()
    }

    pub fn certify_block(
        &self,
        epoch: Epoch,
        height: u64,
        timestamp: u64,
        state_root: B256,
        extra_data: Vec<u8>,
    ) -> CertifiedBlockFixture {
        self.certify_child_block(CertifiedChildBlockInput {
            epoch,
            height,
            timestamp,
            state_root,
            extra_data,
            parent_hash: B256::ZERO,
        })
    }

    pub fn certify_child_block(&self, input: CertifiedChildBlockInput) -> CertifiedBlockFixture {
        let CertifiedChildBlockInput {
            epoch,
            height,
            timestamp,
            state_root,
            extra_data,
            parent_hash,
        } = input;

        let mut block = Block::<outbe_primitives::OutbeTxEnvelope>::default();
        block.header.parent_hash = parent_hash;
        block.header.number = height;
        block.header.timestamp = timestamp;
        block.header.state_root = state_root;
        block.header.extra_data = Bytes::from(extra_data);
        let block =
            ConsensusBlock::from_sealed(SealedBlock::seal_slow(block.map_header(OutbeHeader::new)));

        let namespace = outbe_simplex::config::outbe_app_namespace();
        let verifier = HybridScheme::<MinSig>::verifier_with_vrf_provider(
            &namespace,
            self.participants.clone(),
            VrfMaterialProvider::new(epoch.get(), self.dkg.polynomial.clone(), None),
        )
        .expect("fixture verifier constructs");
        let signers = self
            .keys
            .iter()
            .map(|key| self.signer(key, epoch))
            .collect::<Vec<_>>();
        let proposal = Proposal::new(
            Round::new(epoch, View::new(2)),
            View::new(1),
            block.digest(),
        );
        let subject = Subject::Finalize {
            proposal: &proposal,
        };
        let attestations = signers
            .iter()
            .map(|signer| signer.sign::<Digest>(subject).expect("fixture vote signs"))
            .collect::<Vec<_>>();
        let certificate = verifier
            .assemble(
                commonware_utils::iter::NonEmpty::try_new(attestations.into_iter())
                    .expect("fixture quorum is non-empty"),
                &Sequential,
            )
            .expect("fixture finalization assembles");
        let finalization: Finalization = Finalization {
            proposal,
            certificate,
        };
        CertifiedBlockFixture {
            finalization: finalization.encode().to_vec(),
            block: block.encode().to_vec(),
            block_hash: block.block_hash(),
        }
    }
    fn signer(&self, key: &bls12381::PrivateKey, epoch: Epoch) -> HybridScheme<MinSig> {
        let index = self.participants.index(&key.public_key()).unwrap();
        let material = VrfMaterialProvider::new(
            epoch.get(),
            self.dkg.polynomial.clone(),
            Some(self.dkg.shares[index.get() as usize].clone()),
        );
        HybridScheme::signer_with_vrf_provider(
            &outbe_simplex::config::outbe_app_namespace(),
            self.participants.clone(),
            key.clone(),
            material,
        )
        .unwrap()
    }
}
