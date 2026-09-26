use super::*;
use std::{collections::BTreeMap, convert::Infallible, sync::Arc};

use alloy_primitives::Bytes;
use commonware_codec::Encode as _;
use commonware_consensus::marshal::store::{Blocks, Certificates};
use commonware_consensus::simplex::types::{Finalization, Proposal, Subject};
use commonware_consensus::types::{Epocher as _, Height, Round, View};
use commonware_consensus::Heightable as _;
use commonware_cryptography::certificate::Scheme as _;
use commonware_cryptography::{Hasher as _, Sha256, Signer as _};
use commonware_storage::archive::Identifier;
use commonware_utils::{
    ordered::{Quorum as _, Set as OrderedSet},
    TryCollect as _,
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
                tee_expired_target_exclusions: vec![],
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
        let digest = Digest::from(alloy_primitives::B256::from_slice(
            Sha256::hash(&[format!("blk-{}", epoch.get()).as_bytes()]).as_ref(),
        ));
        self.finalization_for(epoch, digest)
    }

    fn finalization_for(
        &self,
        epoch: Epoch,
        digest: Digest,
    ) -> Finalization<HybridScheme<MinSig>, Digest> {
        let signer_indices: Vec<_> = (0..self.keys.len()).collect();
        self.finalization_for_signers(epoch, digest, &signer_indices)
    }

    fn finalization_for_signers(
        &self,
        epoch: Epoch,
        digest: Digest,
        signer_indices: &[usize],
    ) -> Finalization<HybridScheme<MinSig>, Digest> {
        let proposal = Proposal::new(Round::new(epoch, View::new(2)), View::new(1), digest);
        self.finalization_for_proposal(epoch, proposal, signer_indices)
    }

    fn finalization_for_proposal(
        &self,
        verifier_epoch: Epoch,
        proposal: Proposal<Digest>,
        signer_indices: &[usize],
    ) -> Finalization<HybridScheme<MinSig>, Digest> {
        let ns = crate::config::outbe_app_namespace();
        let verifier = HybridScheme::<MinSig>::verifier_with_vrf_provider(
            &ns,
            self.participants.clone(),
            VrfMaterialProvider::new(verifier_epoch.get(), self.dkg.polynomial.clone(), None),
        )
        .unwrap();
        let signers: Vec<HybridScheme<MinSig>> = self
            .keys
            .iter()
            .map(|key| {
                let idx = self.participants.index(&key.public_key()).unwrap();
                HybridScheme::signer_with_vrf_provider(
                    &ns,
                    self.participants.clone(),
                    key.clone(),
                    VrfMaterialProvider::new(
                        verifier_epoch.get(),
                        self.dkg.polynomial.clone(),
                        Some(self.dkg.shares[idx.get() as usize].clone()),
                    ),
                )
                .unwrap()
            })
            .collect();
        let subject = Subject::Finalize {
            proposal: &proposal,
        };
        let attestations: Vec<_> = signer_indices
            .iter()
            .map(|index| signers[*index].sign::<Digest>(subject).unwrap())
            .collect();
        let certificate = verifier
            .assemble(
                commonware_utils::iter::NonEmpty::try_new(attestations.into_iter()).unwrap(),
                &Sequential,
            )
            .unwrap();
        Finalization {
            proposal,
            certificate,
        }
    }
}

#[derive(Clone, Default)]
struct ArchivedFinalizedSource {
    by_height: Arc<BTreeMap<u64, CertifiedFinalizedBlock>>,
}

impl FinalizedSource for ArchivedFinalizedSource {
    fn get_finalization(
        &self,
        height: Height,
    ) -> impl std::future::Future<Output = Option<CertifiedFinalizedBlock>> + Send {
        std::future::ready(self.by_height.get(&height.get()).cloned())
    }
}

#[derive(Clone, Debug, Default)]
struct MemoryCertificates {
    by_height: Arc<std::sync::Mutex<BTreeMap<u64, crate::marshal_types::Finalization>>>,
}

impl Certificates for MemoryCertificates {
    type BlockDigest = Digest;
    type Commitment = Digest;
    type Scheme = HybridScheme<MinSig>;
    type Error = Infallible;

    async fn has(&self, height: Height) -> Result<bool, Self::Error> {
        Ok(self.by_height.lock().unwrap().contains_key(&height.get()))
    }

    async fn put(
        self,
        height: Height,
        _digest: Self::BlockDigest,
        finalization: crate::marshal_types::Finalization,
    ) -> Result<Self, Self::Error> {
        self.by_height
            .lock()
            .unwrap()
            .entry(height.get())
            .or_insert(finalization);
        Ok(self)
    }

    async fn sync(self) -> Result<Self, Self::Error> {
        Ok(self)
    }

    async fn get(
        &self,
        id: Identifier<'_, Self::BlockDigest>,
    ) -> Result<Option<crate::marshal_types::Finalization>, Self::Error> {
        let value = match id {
            Identifier::Index(height) => self.by_height.lock().unwrap().get(&height).cloned(),
            Identifier::Key(digest) => self
                .by_height
                .lock()
                .unwrap()
                .values()
                .find(|finalization| finalization.proposal.payload == *digest)
                .cloned(),
        };
        Ok(value)
    }

    async fn prune(self, min: Height) -> Result<Self, Self::Error> {
        self.by_height
            .lock()
            .unwrap()
            .retain(|height, _| *height >= min.get());
        Ok(self)
    }

    fn last_index(&self) -> Option<Height> {
        self.by_height
            .lock()
            .unwrap()
            .last_key_value()
            .map(|(height, _)| Height::new(*height))
    }

    fn ranges_from(&self, from: Height) -> impl Iterator<Item = (Height, Height)> {
        self.by_height
            .lock()
            .unwrap()
            .range(from.get()..)
            .map(|(height, _)| (Height::new(*height), Height::new(*height)))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

#[derive(Clone, Debug, Default)]
struct MemoryBlocks {
    by_height: Arc<std::sync::Mutex<BTreeMap<u64, crate::block::ConsensusBlock>>>,
}

impl Blocks for MemoryBlocks {
    type Block = crate::block::ConsensusBlock;
    type Error = Infallible;

    async fn put(self, block: Self::Block) -> Result<Self, Self::Error> {
        self.by_height
            .lock()
            .unwrap()
            .entry(block.height().get())
            .or_insert(block);
        Ok(self)
    }

    async fn sync(self) -> Result<Self, Self::Error> {
        Ok(self)
    }

    async fn get(&self, id: Identifier<'_, Digest>) -> Result<Option<Self::Block>, Self::Error> {
        let value = match id {
            Identifier::Index(height) => self.by_height.lock().unwrap().get(&height).cloned(),
            Identifier::Key(digest) => self
                .by_height
                .lock()
                .unwrap()
                .values()
                .find(|block| block.digest() == *digest)
                .cloned(),
        };
        Ok(value)
    }

    async fn prune(self, min: Height) -> Result<Self, Self::Error> {
        self.by_height
            .lock()
            .unwrap()
            .retain(|height, _| *height >= min.get());
        Ok(self)
    }

    fn missing_items(&self, start: Height, max: usize) -> Vec<Height> {
        let Some(last) = self
            .by_height
            .lock()
            .unwrap()
            .last_key_value()
            .map(|(height, _)| *height)
        else {
            return Vec::new();
        };
        (start.get()..=last)
            .filter(|height| !self.by_height.lock().unwrap().contains_key(height))
            .take(max)
            .map(Height::new)
            .collect()
    }

    fn next_gap(&self, value: Height) -> (Option<Height>, Option<Height>) {
        let current = self
            .by_height
            .lock()
            .unwrap()
            .contains_key(&value.get())
            .then_some(value);
        let next = self
            .by_height
            .lock()
            .unwrap()
            .range(value.get().saturating_add(1)..)
            .next()
            .map(|(height, _)| Height::new(*height));
        (current, next)
    }

    fn last_index(&self) -> Option<Height> {
        self.by_height
            .lock()
            .unwrap()
            .last_key_value()
            .map(|(height, _)| Height::new(*height))
    }
}

#[derive(Clone, Debug, Default)]
struct DurableCrashBlocks {
    durable: Arc<std::sync::Mutex<BTreeMap<u64, crate::block::ConsensusBlock>>>,
    buffered: BTreeMap<u64, crate::block::ConsensusBlock>,
    fail_next_sync: bool,
}

impl Blocks for DurableCrashBlocks {
    type Block = crate::block::ConsensusBlock;
    type Error = std::io::Error;

    async fn put(mut self, block: Self::Block) -> Result<Self, Self::Error> {
        let height = block.height().get();
        if !self.durable.lock().unwrap().contains_key(&height) {
            self.buffered.entry(height).or_insert(block);
        }
        Ok(self)
    }

    async fn sync(mut self) -> Result<Self, Self::Error> {
        if std::mem::take(&mut self.fail_next_sync) {
            return Err(std::io::Error::other("injected block sync crash"));
        }
        self.durable.lock().unwrap().append(&mut self.buffered);
        Ok(self)
    }

    async fn get(&self, id: Identifier<'_, Digest>) -> Result<Option<Self::Block>, Self::Error> {
        let durable = self.durable.lock().unwrap();
        let value = match id {
            Identifier::Index(height) => {
                self.buffered.get(&height).or_else(|| durable.get(&height))
            }
            Identifier::Key(digest) => self
                .buffered
                .values()
                .chain(durable.values())
                .find(|block| block.digest() == *digest),
        };
        Ok(value.cloned())
    }

    async fn prune(mut self, min: Height) -> Result<Self, Self::Error> {
        self.durable
            .lock()
            .unwrap()
            .retain(|height, _| *height >= min.get());
        self.buffered.retain(|height, _| *height >= min.get());
        Ok(self)
    }

    fn missing_items(&self, start: Height, max: usize) -> Vec<Height> {
        let last = self
            .durable
            .lock()
            .unwrap()
            .keys()
            .chain(self.buffered.keys())
            .max()
            .copied();
        let Some(last) = last else {
            return Vec::new();
        };
        (start.get()..=last)
            .filter(|height| {
                !self.durable.lock().unwrap().contains_key(height)
                    && !self.buffered.contains_key(height)
            })
            .take(max)
            .map(Height::new)
            .collect()
    }

    fn next_gap(&self, value: Height) -> (Option<Height>, Option<Height>) {
        let contains = self.durable.lock().unwrap().contains_key(&value.get())
            || self.buffered.contains_key(&value.get());
        let next = self
            .durable
            .lock()
            .unwrap()
            .keys()
            .chain(self.buffered.keys())
            .filter(|height| **height > value.get())
            .min()
            .copied()
            .map(Height::new);
        (contains.then_some(value), next)
    }

    fn last_index(&self) -> Option<Height> {
        self.durable
            .lock()
            .unwrap()
            .keys()
            .chain(self.buffered.keys())
            .max()
            .copied()
            .map(Height::new)
    }
}

fn certified_block(
    signer: &Committee,
    epoch: Epoch,
    height: u64,
    extra_data: Vec<u8>,
) -> CertifiedFinalizedBlock {
    use reth_ethereum::{primitives::SealedBlock, Block};

    let mut block = Block::default();
    block.header.number = height;
    block.header.extra_data = Bytes::from(extra_data);
    let block = crate::block::ConsensusBlock::from_sealed(SealedBlock::seal_slow(
        block.map_header(outbe_primitives::OutbeHeader::new),
    ));
    let finalization = signer.finalization_for(epoch, block.digest());
    CertifiedFinalizedBlock {
        finalization,
        block,
    }
}

fn fill_plain_finalized_range(
    records: &mut BTreeMap<u64, CertifiedFinalizedBlock>,
    signer: &Committee,
    epoch: Epoch,
    heights: impl Iterator<Item = u64>,
) {
    for height in heights {
        records
            .entry(height)
            .or_insert_with(|| certified_block(signer, epoch, height, Vec::new()));
    }
}

mod admission;
mod chain;
mod replay;
mod replay_conflicts;
mod wire;
