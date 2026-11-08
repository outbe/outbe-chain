//! Compile-time registry of vote target-module handlers.

use outbe_coderegistry::vote_target::CodePatchVoteTarget;
use outbe_hyperlanecontroller::HyperlaneVoteTarget;
use outbe_l2registry::L2RegistryVoteTarget;
use outbe_stablecoinfactory::StablecoinFactoryVoteTarget;
use outbe_update::vote_target::UpdateVoteTarget;
use outbe_vaultrouter::VaultRouterVoteTarget;

pub use outbe_primitives::vote_target::{
    decode_proposal_payload, decode_stored_proposal_payload, TargetAdmission, TargetElectorate,
    TargetExecutionOutcome, VoteTarget, VoteTargetContext, VoteTargetError, VoteTargetHandlers,
    VoteTargetRegistry,
};

static UPDATE_VOTE_TARGET: UpdateVoteTarget = UpdateVoteTarget;
static L2_REGISTRY_VOTE_TARGET: L2RegistryVoteTarget = L2RegistryVoteTarget;
static CODE_PATCH_VOTE_TARGET: CodePatchVoteTarget = CodePatchVoteTarget;
static VAULT_ROUTER_VOTE_TARGET: VaultRouterVoteTarget = VaultRouterVoteTarget;
static HYPERLANE_VOTE_TARGET: HyperlaneVoteTarget = HyperlaneVoteTarget;
static STABLECOIN_FACTORY_VOTE_TARGET: StablecoinFactoryVoteTarget = StablecoinFactoryVoteTarget;

/// Active vote target handlers for the node runtime.
pub static ACTIVE_VOTE_TARGETS: VoteTargetHandlers = &[
    &UPDATE_VOTE_TARGET,
    &L2_REGISTRY_VOTE_TARGET,
    &CODE_PATCH_VOTE_TARGET,
    &VAULT_ROUTER_VOTE_TARGET,
    &STABLECOIN_FACTORY_VOTE_TARGET,
    &HYPERLANE_VOTE_TARGET,
];

static REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(ACTIVE_VOTE_TARGETS);

/// Returns the compile-time vote target registry for precompile and begin-block wiring.
#[must_use]
pub fn registry() -> &'static VoteTargetRegistry {
    &REGISTRY
}
