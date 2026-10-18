//! The cucumber `World` and the encapsulated handles the steps drive.
//!
//! Steps never shell out directly. They call verb methods on these handles
//! (`world.localnet.start(...)`, `world.rpc.send_propose(...)`,
//! `world.validators.operator(...)`) and thread scratch values through
//! `world.state`. The handles hold a cloned [`Config`] and defer all subprocess
//! work to `crate::internal`.

pub mod bidders;

pub mod credis;
pub mod forge;
pub mod hardhat;
pub mod localnet;
pub mod origin_venue;
pub mod price_oracle;
pub mod projection;
pub mod relay;
pub mod rpc;

pub mod settlement_currency;
pub mod state;
pub mod target_chain;
pub mod test_issuance;
pub mod validators;
pub mod venue_probes;

use crate::env::environment;
use crate::internal::config::Config;
use localnet::Localnet;
use price_oracle::PriceOracleTopology;
use projection::ProjectionFixture;
use rpc::Rpc;
use state::FixtureState;
use std::time::Instant;
use target_chain::TargetChain;
use validators::Validators;

// Public result types returned by the `Rpc` handle.
pub use crate::internal::parse::{ScheduledUpdate, VoteStatus};
// The committee's primary RPC port lives with the `Validators` handle.

#[derive(Debug, cucumber::World)]
pub struct World {
    pub(crate) started_at: Instant,
    /// The localnet and every owned node: bootstrap/start/stop the committee,
    /// provision/launch the joiner + followers, kill/restart validators.
    pub localnet: Localnet,
    /// Fixed-checkpoint EVM state reads over RPC, compared across validators.
    pub projection: ProjectionFixture,
    /// Chain reads/sends/waits.
    pub rpc: Rpc,
    /// Validator/operator identities and committee size.
    pub validators: Validators,
    /// Harness-owned price source and the real feeder process that consumes it.
    pub price_oracle: PriceOracleTopology,
    /// Local EVM chain the committee bridges to. Idle until a scenario starts it.
    pub target_chain: TargetChain,
    /// Carries mailbox dispatches between the two chains. Dropping it stops the pump,
    /// so it lives with the scenario rather than with either chain.
    pub relay: Option<crate::world::relay::Relay>,
    /// Scratch state threaded across the scenario's steps.
    pub state: FixtureState,
}

impl Default for World {
    fn default() -> Self {
        let env = environment();
        let id = crate::env::next_scenario_id();
        // Before the `Config`, whose `rpc0` reads this scenario's validator-0 port.
        env.ports
            .start_scenario(env.validators)
            .expect("allocate this scenario's port blocks");
        let cfg = Config::for_scenario(&env, id);
        cfg.validate_committee_ipc_paths(env.validators)
            .expect("validate scenario node IPC paths before starting services");
        let projection = projection::projection_fixture(&cfg);
        let target_chain = TargetChain::new(cfg.clone());
        Self {
            relay: None,
            started_at: Instant::now(),
            localnet: Localnet::new(cfg.clone()),
            projection,
            rpc: Rpc::new(cfg.clone()),
            validators: Validators::new(cfg.clone(), env.validators),
            price_oracle: PriceOracleTopology::new(cfg),
            target_chain,
            state: FixtureState::default(),
        }
    }
}
