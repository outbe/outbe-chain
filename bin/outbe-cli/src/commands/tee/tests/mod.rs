use super::identity::authorize_validator_node_binding;
use super::identity::load_secp256k1_key_file;
use super::identity::parse_nonzero_b256;
use super::join::classify_join_offer_key_state;
use super::join::ensure_durable_join_registration_caller;
use super::join::ensure_joinable_binding;
use super::join::finalized_join_admission_anchor_v1;

use super::join::plan_join_completion;
use super::join::plan_missing_committed_relay;
use super::join::registration_counters;
use super::join::relay_exact_join_transaction;
use super::join::run_finalized_admission_recovery_v1;
use super::join::select_join_transport;
use super::join::ExactJoinRelayV1;
use super::join::FinalizedAdmissionAttemptErrorV1;
use super::join::FinalizedAdmissionRecoveryIoV1;
use super::join::JoinCompletionPlan;
use super::join::JoinOfferKeyState;
use super::join::JoinTransport;
use super::join::MissingCommittedRelayPlan;

use alloy_primitives::keccak256;
use alloy_primitives::Address;
use alloy_primitives::B256;
use alloy_primitives::U256;

use outbe_operator::tee::RenewalBindingV1;

use outbe_primitives::tee_attestation_v1::AttestationMode;

use outbe_tee::protocol::EnclaveResponse;

use outbe_tee::FinalizedJoinAdmissionAnchorV1;
use outbe_tee::FinalizedRegistryViewV1;

use outbe_tee::TransportError;
use std::fs;

use outbe_rpc::test_support::{
    ExpectedRpcCall, RecordedRpcCall, RecordedRpcResponse, RecordingRpc,
};
use std::collections::VecDeque;

mod admission;

mod identity;

mod join_policy;
