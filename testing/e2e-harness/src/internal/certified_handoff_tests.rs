use super::*;
use commonware_codec::Encode as _;
use commonware_consensus::simplex::types::{Proposal, Subject};
use commonware_consensus::types::{Round, View};
use commonware_cryptography::{
    bls12381::primitives::variant::MinSig, certificate::Scheme as _, Signer as _,
};
use commonware_parallel::Sequential;
use commonware_utils::{
    ordered::{Quorum as _, Set},
    N3f1,
};
use outbe_consensus::{
    block::ConsensusBlock,
    bls::{bootstrap_dkg_for_participants, ParticipantDkgBootstrapResult},
    digest::Digest,
    dkg_manager::{build_boundary_artifact, BoundaryArtifactInput},
    hybrid::{HybridScheme, VrfMaterialProvider},
    marshal_types::Finalization,
};
use outbe_primitives::{
    reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts},
    validators::ValidatorP2pAddress,
    OutbeHeader,
};
use reth_ethereum::{primitives::SealedBlock, Block};

/// A real private test committee. Unlike the existing single-block fixture,
/// these blocks bind consecutive parent hashes to exercise history traversal.
struct Committee {
    keys: Vec<bls12381::PrivateKey>,
    players: Set<bls12381::PublicKey>,
    dkg: ParticipantDkgBootstrapResult,
}

impl Committee {
    fn new(seed: u64) -> Self {
        let mut keys = (1..=4)
            .map(|n| bls12381::PrivateKey::from_seed(seed + n))
            .collect::<Vec<_>>();
        keys.sort_by_key(|key| key.public_key().encode());
        let players: Set<_> = keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();
        let dkg = bootstrap_dkg_for_participants(players.clone()).unwrap();
        Self { keys, players, dkg }
    }

    fn binding(&self) -> DcapSeededChainSpecBindingV1 {
        DcapSeededChainSpecBindingV1 {
            chain_id: 54322345,
            genesis_hash: B256::ZERO,
            genesis_consensus_keys: self
                .players
                .iter()
                .map(|key| key.encode().as_ref().try_into().unwrap())
                .collect(),
        }
    }

    fn outcome(&self, epoch: u64) -> Bytes {
        let ConsensusHeaderArtifact::BoundaryOutcome(boundary) = self.boundary(epoch) else {
            unreachable!("fixture builds a boundary")
        };
        boundary.outcome
    }

    fn boundary(&self, epoch: u64) -> ConsensusHeaderArtifact {
        let validators = outbe_consensus::validators::ValidatorSet {
            public_keys: self.players.iter().cloned().collect(),
            addresses: (1..=4)
                .map(alloy_primitives::Address::repeat_byte)
                .collect(),
            p2p_addresses: vec![ValidatorP2pAddress::Missing; 4],
        };
        ConsensusHeaderArtifact::BoundaryOutcome(
            build_boundary_artifact(BoundaryArtifactInput {
                epoch: Epoch::new(epoch),
                validator_set: &validators,
                output: &self.dkg.output,
                is_full_dkg: false,
                dkg_cycle: 1,
                freeze_height: 2,
                planned_activation_height: 3,
                vrf_material_version: epoch,
                is_validator_set_change: false,
                tee_expired_target_exclusions: vec![],
            })
            .unwrap(),
        )
    }

    fn preannounce(&self, epoch: u64) -> ConsensusHeaderArtifact {
        ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch,
            outcome: self.outcome(epoch),
        }
    }

    fn certified(
        &self,
        epoch: u64,
        height: u64,
        parent: B256,
        artifact: Option<ConsensusHeaderArtifact>,
    ) -> (CertifiedFinalizedBlock, FinalizedCheckpoint) {
        let mut block = Block::default();
        block.header.number = height;
        block.header.parent_hash = parent;
        block.header.state_root = B256::repeat_byte(0x42);
        block.header.extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            consensus_header_artifact: artifact,
            ..Default::default()
        })
        .unwrap();
        let block =
            ConsensusBlock::from_sealed(SealedBlock::seal_slow(block.map_header(OutbeHeader::new)));
        let namespace = outbe_consensus::config::outbe_app_namespace();
        let verifier = HybridScheme::<MinSig>::verifier_with_vrf_provider(
            &namespace,
            self.players.clone(),
            VrfMaterialProvider::new(epoch, self.dkg.polynomial.clone(), None),
        )
        .unwrap();
        let proposal = Proposal::new(
            Round::new(Epoch::new(epoch), View::new(height + 1)),
            View::new(height),
            block.digest(),
        );
        let subject = Subject::Finalize {
            proposal: &proposal,
        };
        let attestations = self
            .keys
            .iter()
            .map(|key| {
                let index = self.players.index(&key.public_key()).unwrap();
                let signer = HybridScheme::<MinSig>::signer_with_vrf_provider(
                    &namespace,
                    self.players.clone(),
                    key.clone(),
                    VrfMaterialProvider::new(
                        epoch,
                        self.dkg.polynomial.clone(),
                        Some(self.dkg.shares[index.get() as usize].clone()),
                    ),
                )
                .unwrap();
                signer.sign::<Digest>(subject).unwrap()
            })
            .collect::<Vec<_>>();
        let certificate = verifier
            .assemble::<_, N3f1>(attestations, &Sequential)
            .unwrap();
        let finalization: Finalization = Finalization {
            proposal,
            certificate,
        };
        let expected = FinalizedCheckpoint {
            height,
            block_hash: block.block_hash(),
            state_root: block.header().inner.state_root,
        };
        let decoded = decode_public_finalized_block(
            &finalization.encode(),
            &block.encode(),
            self.players.len(),
        )
        .unwrap();
        (decoded, expected)
    }
}

fn anchored(committee: &Committee) -> AuthenticatedHistory {
    let mut history = AuthenticatedHistory::new(&committee.binding()).unwrap();
    let (proof, expected) = committee.certified(0, 1, B256::ZERO, Some(committee.boundary(0)));
    history.advance(&proof, expected).unwrap();
    history
}

#[test]
fn real_certificates_chain_and_recheck_one_exact_pinned_handoff() {
    let c0 = Committee::new(10);
    let c1 = Committee::new(30);
    let mut history = anchored(&c0);
    let (pre, carrier) = c0.certified(0, 2, history.last_hash, Some(c1.preannounce(1)));
    history.advance(&pre, carrier).unwrap();
    let (boundary, checkpoint) = c1.certified(1, 3, carrier.block_hash, Some(c1.boundary(1)));
    history.advance(&boundary, checkpoint).unwrap();
    let mut pin = PinnedHandoff {
        history,
        epoch: 1,
        carrier,
        outcome: c1.outcome(1),
        boundary: Some(checkpoint),
        follower_pids: [1, 2],
        survivor_anchor_after_fault: None,
        follower_watermarks: None,
    };
    pin.verify_carrier(&pre).unwrap();
    pin.verify_boundary(&boundary).unwrap();
    let (self_certified, same_carrier) =
        c1.certified(1, 2, pre.block.parent_hash(), Some(c1.preannounce(1)));
    assert_eq!(same_carrier, carrier, "only the certificate was replaced");
    assert!(pin.verify_carrier(&self_certified).is_err());
    assert!(pin
        .history
        .verify_retained(&self_certified, carrier)
        .is_err());
    pin.require_before_boundary(&pre.block.header().inner.extra_data)
        .unwrap();
    assert!(pin
        .require_before_boundary(&boundary.block.header().inner.extra_data)
        .is_err());
    assert!(pin.require_before_boundary(b"corrupt artifact").is_err());
    pin.outcome = c0.outcome(1);
    assert!(pin.verify_carrier(&pre).is_err());
    assert!(pin.verify_boundary(&boundary).is_err());
    pin.outcome = c1.outcome(1);
    pin.epoch = 2;
    assert!(pin.verify_carrier(&pre).is_err());
    assert!(pin.verify_boundary(&boundary).is_err());
}

#[test]
fn missing_preannounce_cannot_be_replaced_by_self_certified_successor_boundary() {
    let c0 = Committee::new(10);
    let c1 = Committee::new(30);
    let mut history = anchored(&c0);
    let (proof, expected) = c1.certified(1, 2, history.last_hash, Some(c1.boundary(1)));
    assert!(history.advance(&proof, expected).is_err());
}

#[test]
fn skipped_epoch_and_conflicting_preannounce_fail_even_with_valid_old_committee_signatures() {
    let c0 = Committee::new(10);
    let c1 = Committee::new(30);
    let mut history = anchored(&c0);
    let (proof, expected) = c0.certified(0, 2, history.last_hash, Some(c1.preannounce(2)));
    assert!(history.advance(&proof, expected).is_err());
    let mut history = anchored(&c0);
    let (proof, expected) = c0.certified(0, 2, history.last_hash, Some(c1.preannounce(1)));
    history.advance(&proof, expected).unwrap();
    let (conflict, expected) = c0.certified(0, 3, history.last_hash, Some(c0.preannounce(1)));
    assert!(history.advance(&conflict, expected).is_err());
}

#[test]
fn boundary_cannot_override_a_previously_authenticated_committee() {
    let c0 = Committee::new(10);
    let c1 = Committee::new(30);
    let attacker = Committee::new(50);
    let mut history = anchored(&c0);
    let (pre, expected) = c0.certified(0, 2, history.last_hash, Some(c1.preannounce(1)));
    history.advance(&pre, expected).unwrap();
    let (wrong, expected) = attacker.certified(1, 3, history.last_hash, Some(attacker.boundary(1)));
    assert!(history.advance(&wrong, expected).is_err());
}

#[test]
fn real_certificate_does_not_bless_wrong_height_parent_root_hash_or_payload() {
    let c0 = Committee::new(10);
    let mut history = anchored(&c0);
    let (proof, expected) = c0.certified(0, 2, history.last_hash, None);
    assert!(history.verify_retained(&proof, expected).is_err());
    history.advance(&proof, expected).unwrap();
    history.verify_retained(&proof, expected).unwrap();
    for wrong in [
        FinalizedCheckpoint {
            height: 3,
            ..expected
        },
        FinalizedCheckpoint {
            block_hash: B256::ZERO,
            ..expected
        },
        FinalizedCheckpoint {
            state_root: B256::ZERO,
            ..expected
        },
    ] {
        assert!(history.verify_retained(&proof, wrong).is_err());
    }
    let mut wrong = proof.clone();
    wrong.finalization.proposal.payload = Digest(B256::ZERO);
    assert!(history.verify_retained(&wrong, expected).is_err());
    let mut history = anchored(&c0);
    let (wrong_parent, expected) = c0.certified(0, 2, B256::repeat_byte(9), None);
    assert!(history.advance(&wrong_parent, expected).is_err());
}

#[test]
fn certificate_from_another_genesis_committee_and_truncated_wire_fail() {
    let c0 = Committee::new(10);
    let other = Committee::new(30);
    let mut history = AuthenticatedHistory::new(&c0.binding()).unwrap();
    let (wrong, expected) = other.certified(0, 1, B256::ZERO, Some(other.boundary(0)));
    assert!(history.advance(&wrong, expected).is_err());
    let bytes = wrong.finalization.encode();
    assert!(
        decode_public_finalized_block(&bytes[..bytes.len() - 1], &wrong.block.encode(), 4).is_err()
    );
    let mut trailing = bytes.to_vec();
    trailing.push(0);
    assert!(decode_public_finalized_block(&trailing, &wrong.block.encode(), 4).is_err());
}
