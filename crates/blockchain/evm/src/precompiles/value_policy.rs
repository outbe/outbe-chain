//! Native-value admission for an already validated EVM call frame.
use super::routes::ValuePolicy;
use alloy_primitives::U256;
use evm2::interpreter::{MessageExt, MessageKind};

/// Call-boundary value semantics derived from an evm2 call message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BoundaryCallValue {
    /// Native value transferred by a non-delegated `CALL` frame.
    Transfer(U256),
    /// Apparent value inherited by a delegated frame without moving funds.
    Apparent(U256),
}

impl BoundaryCallValue {
    pub(super) fn from_message<E>(message: &MessageExt<E>) -> Self {
        match message.kind {
            MessageKind::DelegateCall => Self::Apparent(message.value),
            MessageKind::StaticCall => Self::Transfer(U256::ZERO),
            MessageKind::Call | MessageKind::CallCode => Self::Transfer(message.value),
            _ => Self::Apparent(message.value),
        }
    }
}

/// Classification of a call's native value at the precompile boundary.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BoundaryValue {
    /// Value the EVM has already moved into the precompile account, safe to credit.
    Credited(U256),
    /// Value that must not reach dispatch, with the reason to revert with.
    Rejected(&'static str),
}

/// Decide how much native value a precompile call may credit.
///
/// Only a non-delegated frame reaches this: dispatch refuses `DELEGATECALL` and
/// `CALLCODE` outright, so the EVM has already moved any `Transfer` value into
/// the account whose storage dispatch is about to mutate.
///
/// The `BoundaryCallValue::Apparent` arm is therefore unreachable. It is not a fallback
/// for the delegated-frame guard and must not be read as one. `CALLCODE` carries
/// a `Transfer`. If someone removes that guard, this function would credit a
/// self-transfer that moved nothing. The guard is the only thing standing there.
///
/// A route that declares `ValuePolicy::Reject` then refuses any credited amount,
/// so a call that would strand funds at a precompile stops before touching state.
/// SELFDESTRUCT cannot strand value at an exact route either: the EVM sends it to
/// address 0, and the Promis Limit burns it.
pub(super) fn classify_boundary_value(
    policy: ValuePolicy,
    value: &BoundaryCallValue,
) -> BoundaryValue {
    let credited = match *value {
        BoundaryCallValue::Transfer(v) | BoundaryCallValue::Apparent(v) => v,
    };
    if credited.is_zero() {
        return BoundaryValue::Credited(U256::ZERO);
    }
    if matches!(*value, BoundaryCallValue::Apparent(_)) {
        return BoundaryValue::Rejected("outbe precompile: apparent value is not a transfer");
    }
    if policy == ValuePolicy::Reject {
        return BoundaryValue::Rejected("outbe precompile: non-payable address called with value");
    }
    BoundaryValue::Credited(credited)
}
