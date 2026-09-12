use alloy_primitives::{address, keccak256, Address, B256, U256};
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
use outbe_ocomp_protocol::{
    committee::{
        validator_identity_hash_v1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        POC_KEY_EPOCH, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    profile::poc_schema_limits,
};
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

mod fixtures;
use fixtures::{
    activate_for_test, activate_staked_for_test, confirm_ready, dummy_consensus_pubkey,
    make_inactive_for_test, ocomp_registration, with_vs_configured, CHAIN_ID, OWNER,
};

mod boundary;
mod identity;
mod lifecycle;
mod ocomp_recovery;
mod operational_key_delegation;
mod participation;
mod punishment;
mod readiness;
mod registration;

// ---- Step 8: idempotent record_finalized_participation hook tests --------
mod record_finalized_participation_idempotency;
