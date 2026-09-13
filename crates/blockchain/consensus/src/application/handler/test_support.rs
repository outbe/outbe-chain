#[cfg(test)]
use super::validate_header_consensus_artifacts_for_activation;
#[cfg(test)]
use super::ValidatorRole;

use crate::block::ConsensusBlock;
use crate::committee_provider::CommitteeProvider;

use crate::dkg_manager::AncestryReader;

use crate::hybrid::HybridSchemeProvider;

use commonware_consensus::types::Round;

use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_cryptography::bls12381::PublicKey;

use outbe_primitives::system_tx::OcompLifecycleActivation;

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) async fn validate_header_consensus_artifacts(
    block: &ConsensusBlock,
    parent_block: Option<&ConsensusBlock>,
    round: Round,
    proposer: &PublicKey,
    chain_id: u64,
    role: ValidatorRole,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    committee_provider: &CommitteeProvider,
    dkg_manager: &crate::dkg_manager::Mailbox,
    ancestry: &impl AncestryReader,
) -> Result<(), String> {
    validate_header_consensus_artifacts_for_activation(
        block,
        parent_block,
        round,
        proposer,
        chain_id,
        OcompLifecycleActivation::Disabled,
        role,
        certificate_scheme_provider,
        committee_provider,
        dkg_manager,
        ancestry,
    )
    .await
}
