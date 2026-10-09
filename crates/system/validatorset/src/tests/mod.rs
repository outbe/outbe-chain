use alloy_primitives::{address, keccak256, Address, B256, U256};
use outbe_ocomp_protocol::{committee::OcompKeyRegistrationV1, profile::poc_schema_limits};
use outbe_primitives::consensus_p2p::{
    encode_v1, P2pAddress, P2pIngress, MAX_P2P_ADDRESS_ENCODED_LEN, P2P_ADDRESS_VERSION_V1,
};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::validators::{validator_registration_message, VALIDATOR_REGISTRATION_DST};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::runtime::status;
use crate::schema::ValidatorSet;
use crate::state_machine::{StakeProjection, ValidatorLifecycle};
use crate::test_support::StorageOverrides;

mod fixtures;
use fixtures::{
    activate_for_test, activate_staked_for_test, at_height, configured_storage, confirm_ready,
    dummy_consensus_pubkey, make_inactive_for_test, ocomp_registration, register_boundary_active,
    register_participant, register_validators, registry_storage, with_vs_configured, CHAIN_ID,
    OWNER,
};

mod aggregate_seam;
mod boundary;
mod boundary_transitions;
mod identity;
mod lifecycle;
mod ocomp_recovery;
mod operational_key_delegation;
mod participation;
mod precompile_routes;
mod punishment;
mod readiness;
mod registration;
mod registration_precedence;
mod snapshot_rules;
mod transition_rules;

// ---- Step 8: idempotent record_finalized_participation hook tests --------
mod record_finalized_participation_idempotency;
