use alloy_primitives::{Address, U256};
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyRegistryError {
    #[error("invalid policy type {policy_type}")]
    InvalidPolicyType { policy_type: u8 },

    #[error("unknown policy {policy_id}")]
    UnknownPolicy { policy_id: U256 },

    #[error("policy {policy_id} is immutable")]
    ImmutablePolicy { policy_id: U256 },

    #[error("policy admin must be non-zero, got {admin}")]
    InvalidPolicyAdmin { admin: Address },

    #[error("caller {caller} is not admin of policy {policy_id}")]
    UnauthorizedPolicyAdmin { policy_id: U256, caller: Address },

    #[error("policy {policy_id} is not a valid directional child")]
    InvalidDirectionalChild { policy_id: U256 },

    #[error("directional policy {policy_id} has no direct membership")]
    DirectionalMembershipUnsupported { policy_id: U256 },

    #[error("policy member must be non-zero, got {account}")]
    InvalidMember { account: Address },

    #[error("member batch must not be empty")]
    EmptyMemberBatch,

    #[error("member batch length {length} exceeds maximum {maximum}")]
    MemberBatchTooLarge { length: usize, maximum: usize },

    #[error("duplicate member {account}")]
    DuplicateMember { account: Address },

    #[error("membership of {account} in policy {policy_id} is already {member}")]
    MembershipUnchanged {
        policy_id: U256,
        account: Address,
        member: bool,
    },

    #[error("invalid pending policy admin {candidate}")]
    InvalidPendingPolicyAdmin { candidate: Address },

    #[error("caller {caller} is not pending admin of policy {policy_id}")]
    NotPendingPolicyAdmin { policy_id: U256, caller: Address },

    #[error("policy id space is exhausted")]
    PolicyIdExhausted,

    #[error("list limit {limit} must be between 1 and {maximum}")]
    InvalidListLimit { limit: U256, maximum: usize },

    #[error("policy {policy_id} of type {policy_type} cannot enumerate members")]
    PolicyMemberEnumerationUnsupported { policy_id: U256, policy_type: u8 },
}

impl From<PolicyRegistryError> for PrecompileError {
    fn from(error: PolicyRegistryError) -> Self {
        PrecompileError::Revert(error.to_string())
    }
}
