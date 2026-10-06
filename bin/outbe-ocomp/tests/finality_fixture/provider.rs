use std::collections::BTreeMap;

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256};
use outbe_consensus::block::ConsensusBlock;
use outbe_node::ocomp::finality::{PublicAccountProofV1, PublicBlockViewV1};
use outbe_ocomp_protocol::{
    intent::{FinalizedIntentProofV1, JobIntentV1},
    opening::OpeningSubjectsV1,
};
use reth_chainspec::ChainInfo;
use reth_storage_api::{
    errors::provider::ProviderResult, BlockHashReader, BlockIdReader, BlockNumReader,
};

/// Authenticated inputs generated for one exact process-test intent.
pub struct FinalizedIntentProofFixture {
    pub intent: JobIntentV1,
    pub intent_id: B256,
    pub job_id: B256,
    pub proof: FinalizedIntentProofV1,
    pub state_root: B256,
    pub header_hash: B256,
    pub block: ConsensusBlock,
    pub canonical_history: CanonicalHistoryFixture,
    pub public_exact_block: PublicExactBlockFixtureV1,
}

#[derive(Clone)]
pub struct PublicExactBlockFixtureV1 {
    pub finalization_bytes: Vec<u8>,
    pub block_bytes: Vec<u8>,
    pub block_view: PublicBlockViewV1,
    pub intent_id: B256,
    pub canonical_job_record: Vec<u8>,
    pub account_proofs: BTreeMap<Address, PublicAccountProofV1>,
}

/// Finalized JobIntent plus an exact historical state provider for the
/// production Fidelity/Oracle opening builder.
pub struct FinalizedLysisInputFixture {
    pub finalized: FinalizedIntentProofFixture,
    pub opening_provider: LysisOpeningProvider,
    pub subjects: OpeningSubjectsV1,
}

pub type LysisOpeningProvider = outbe_node::test_utils::OpeningProviderFixture;

pub(crate) type LysisOpeningState = outbe_node::test_utils::OpeningStateFixture;

/// Minimal exact canonical-history provider used by the production containment
/// adapter in process tests.
#[derive(Clone)]
pub struct CanonicalHistoryFixture {
    hashes: BTreeMap<u64, B256>,
    finalized: BlockNumHash,
}

impl CanonicalHistoryFixture {
    #[must_use]
    pub fn new(block_number: u64, block_hash: B256) -> Self {
        Self {
            hashes: BTreeMap::from([(block_number, block_hash)]),
            finalized: BlockNumHash::new(block_number, block_hash),
        }
    }

    #[must_use]
    pub fn with_canonical_block(mut self, block_number: u64, block_hash: B256) -> Self {
        self.hashes.insert(block_number, block_hash);
        if block_number > self.finalized.number {
            self.finalized = BlockNumHash::new(block_number, block_hash);
        }
        self
    }
}

impl BlockHashReader for CanonicalHistoryFixture {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok(self.hashes.get(&number).copied())
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok((start..end)
            .filter_map(|number| self.hashes.get(&number).copied())
            .collect())
    }
}

impl BlockNumReader for CanonicalHistoryFixture {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        Ok(ChainInfo {
            best_hash: self.finalized.hash,
            best_number: self.finalized.number,
        })
    }

    fn best_block_number(&self) -> ProviderResult<u64> {
        Ok(self.finalized.number)
    }

    fn last_block_number(&self) -> ProviderResult<u64> {
        Ok(self.finalized.number)
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        Ok(self
            .hashes
            .iter()
            .find_map(|(number, candidate)| (*candidate == hash).then_some(*number)))
    }
}

impl BlockIdReader for CanonicalHistoryFixture {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }
}
