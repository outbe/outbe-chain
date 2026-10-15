//! Carries one canonical voting-open state into either terminal scenario.
use super::super::*;

pub(in crate::lifecycle) struct VotingOpenScenario {
    pub(in crate::lifecycle) environment: OcompSuccessorEnvironment,
    pub(in crate::lifecycle) state: VotingOpenState,
}

pub(in crate::lifecycle) struct OcompSuccessorEnvironment {
    pub(in crate::lifecycle) chain_spec: Arc<ChainSpec<OutbeHeader>>,
    pub(in crate::lifecycle) signer: Arc<OutbeEvmSigner>,
    pub(in crate::lifecycle) runtime_body_readers: RuntimeBodyReaders,
    pub(in crate::lifecycle) fork_install: Arc<outbe_metadosis::config::OcompForkInstallV1>,
    pub(in crate::lifecycle) dkg: Dkg,
    pub(in crate::lifecycle) snapshot: CommitteeSnapshot,
}

impl OcompSuccessorEnvironment {
    pub(in crate::lifecycle) fn fixture<'a>(
        &'a self,
        tree_service: &'a Arc<CompressedTreeService>,
    ) -> OcompSuccessorFixture<'a> {
        OcompSuccessorFixture {
            chain_spec: &self.chain_spec,
            tree_service,
            signer: &self.signer,
            runtime_body_readers: &self.runtime_body_readers,
            fork_install: &self.fork_install,
            dkg: &self.dkg,
            snapshot: &self.snapshot,
        }
    }
}

pub(in crate::lifecycle) struct VotingOpenState {
    pub(in crate::lifecycle) prepared: PreparedParent,
    pub(in crate::lifecycle) proposer: Address,
    pub(in crate::lifecycle) open_height: u64,
    pub(in crate::lifecycle) intent_id: B256,
    pub(in crate::lifecycle) finalized_record: OcompJobRecordV1,
    pub(in crate::lifecycle) voting_open: CanonicalOcompSuccessor,
}

impl VotingOpenScenario {
    pub(in crate::lifecycle) fn into_successor_parts(
        self,
    ) -> (OcompSuccessorEnvironment, VotingOpenState) {
        (self.environment, self.state)
    }
}
