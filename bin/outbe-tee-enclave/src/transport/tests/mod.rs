use super::*;
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::NetworkBindingV1;

mod dkg;
mod domain_requests;
mod fixtures;
mod harness;
mod offer_key;
mod onboarding;
mod session;

use fixtures::{
    evm_signer, honest_announces, install_tribute_offer_key, open_on, owner_sig,
    production_dcap_state, sealed_test_binding, signed_initialization_manifest,
    signed_initialization_manifest_for_mode, testnet_chain_word,
};

use harness::{persist_test_offer_key, resident_enclave, spawn_production_connection, Enclave};

#[cfg(all(feature = "native-dcap", target_arch = "x86_64", target_os = "linux"))]
use fixtures::intent_bound_processor_fixture_wire_bytes;
