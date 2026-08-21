//! The cucumber `World` and the encapsulated handles the steps drive.
//!
//! Steps never shell out directly — they call verb methods on these handles
//! (`world.localnet.start(...)`, `world.rpc.send_propose(...)`,
//! `world.validators.operator(...)`) and thread scratch values through
//! `world.state`. The handles hold a cloned [`Config`] and defer all subprocess
//! work to `crate::internal`.

pub mod bidders;
pub mod forge;
pub mod hardhat;
pub mod localnet;
pub mod mongodb;
pub mod ocomp;
pub mod origin_venue;
pub mod rpc;
pub mod state;
pub mod target_chain;
pub mod validators;
pub mod venue_probes;

use crate::env::environment;
use crate::internal::config::Config;
use crate::ocomp_capacity::OcompCapacityResourceMeterV1;
use localnet::Localnet;
use mongodb::MongoDb;
use ocomp::OcompTopology;
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
    /// Projection database, either supplied by the caller or owned by this scenario.
    pub mongodb: MongoDb,
    /// Chain reads/sends/waits.
    pub rpc: Rpc,
    /// Validator/operator identities and committee size.
    pub validators: Validators,
    /// One isolated OCOMP domain per active validator. Processes are owned here
    /// so dropping a scenario cannot orphan compute work.
    pub ocomp: OcompTopology,
    /// Present only when the Rust capacity runner explicitly starts this
    /// scenario inside its dedicated cold-run cgroup.
    pub(crate) capacity_meter: Option<OcompCapacityResourceMeterV1>,
    /// Local EVM chain the committee bridges to. Idle until a scenario starts it.
    pub target_chain: TargetChain,
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
        let mut cfg = Config::for_scenario(&env, id);
        let capacity_meter = std::env::var("OUTBE_OCOMP_CAPACITY_RUN_ID")
            .ok()
            .map(|run_id| {
                OcompCapacityResourceMeterV1::start_current_process(&run_id).unwrap_or_else(
                    |error| panic!("start dedicated OCOMP capacity meter: {error:#}"),
                )
            });
        let mongodb = MongoDb::connect_or_start(&mut cfg).expect("prepare projection MongoDB");
        let target_chain = TargetChain::new(cfg.clone());
        Self {
            started_at: Instant::now(),
            localnet: Localnet::new(cfg.clone()),
            mongodb,
            rpc: Rpc::new(cfg.clone()),
            validators: Validators::new(cfg.clone(), env.validators),
            ocomp: OcompTopology::new(cfg),
            capacity_meter,
            target_chain,
            state: FixtureState::default(),
        }
    }
}
