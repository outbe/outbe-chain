//! Carries one canonical voting-open state into either terminal scenario.
use super::super::*;

pub(in crate::lifecycle) struct VotingOpenScenario {
    pub(in crate::lifecycle) chain_spec: Arc<ChainSpec<OutbeHeader>>,
    pub(in crate::lifecycle) prepared: PreparedParent,
    pub(in crate::lifecycle) signer: Arc<OutbeEvmSigner>,
    pub(in crate::lifecycle) runtime_body_readers: RuntimeBodyReaders,
    pub(in crate::lifecycle) fork_install: Arc<outbe_metadosis::config::OcompForkInstallV1>,
    pub(in crate::lifecycle) dkg: Dkg,
    pub(in crate::lifecycle) snapshot: CommitteeSnapshot,
    pub(in crate::lifecycle) proposer: Address,
    pub(in crate::lifecycle) open_height: u64,
    pub(in crate::lifecycle) intent_id: B256,
    pub(in crate::lifecycle) finalized_record: OcompJobRecordV1,
    pub(in crate::lifecycle) voting_open: CanonicalOcompSuccessor,
}
