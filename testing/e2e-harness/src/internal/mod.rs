//! Private implementation details behind the `world/` handles: env config,
//! the `xshell` subprocess wrapper, precompile addresses, and output parsers.
//! Nothing here is part of the public handle API.

pub(crate) mod addresses;

pub(crate) mod auction_reference;
pub(crate) mod certified_handoff;
pub(crate) mod config;

pub(crate) mod economic_reference;
pub(crate) mod eth;
pub(crate) mod l2_fixture;
pub(crate) mod launch_log;

pub(crate) mod nod_keys;

pub(crate) mod parse;
pub(crate) mod pending_dkg;
pub(crate) mod ports;

pub(crate) mod pricing_coverage;
pub(crate) mod proc;
pub(crate) mod shell;
pub(crate) mod startup_rejection;

pub(crate) mod state_observation;
pub(crate) mod tribute_keys;
