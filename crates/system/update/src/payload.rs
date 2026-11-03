//! Vote payloads of the Update precompile. Each payload is in the
//! `vote_target` file of its action. This module keeps the public paths.

pub use crate::vote_target::contract_upgrade::{
    ContractUpgrade, UpgradeContractsPayload, UPGRADE_CONTRACTS_OPERATION,
};
pub use crate::vote_target::enclave::{
    RevokeEnclaveBindingPayloadV1, REVOKE_ENCLAVE_BINDING_ACTION_V1,
};
pub use crate::vote_target::protocol_update::{
    decode_schedule_update_json, encode_schedule_update_json, validate_schedule_update_json,
    ScheduleUpdatePayload,
};
pub use crate::vote_target::UpdateVotePayload;
