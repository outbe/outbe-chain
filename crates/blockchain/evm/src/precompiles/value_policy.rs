//! Native-value admission for an already validated EVM call frame.
use crate::precompile_routes::ValuePolicy;
use alloy_primitives::U256;

/// Classification of a call's native value at the precompile boundary.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BoundaryValue {
    /// Value revm has already moved into the precompile account, safe to credit.
    Credited(U256),
    /// Value that must not reach dispatch, with the reason to revert with.
    Rejected(&'static str),
}

/// Decide how much native value a precompile call may credit.
///
/// Only a non-delegated frame reaches this: dispatch refuses `DELEGATECALL` and
/// `CALLCODE` outright, so revm has already moved any `CallValue::Transfer` into
/// the account whose storage dispatch is about to mutate.
///
/// The `CallValue::Apparent` arm is therefore unreachable. It is not a fallback
/// for the delegated-frame guard and must not be read as one: `CALLCODE` carries
/// a `Transfer`, so removing that guard would leave this function crediting a
/// self-transfer that moved nothing. The guard is the only thing standing there.
///
/// A route that declares `ValuePolicy::Reject` then refuses any credited amount,
/// so a call that would strand funds at a precompile stops before touching state.
pub(super) fn classify_boundary_value(
    policy: ValuePolicy,
    value: &revm::interpreter::CallValue,
) -> BoundaryValue {
    let credited = match *value {
        revm::interpreter::CallValue::Transfer(v) | revm::interpreter::CallValue::Apparent(v) => v,
    };
    if credited.is_zero() {
        return BoundaryValue::Credited(U256::ZERO);
    }
    if matches!(*value, revm::interpreter::CallValue::Apparent(_)) {
        return BoundaryValue::Rejected("outbe precompile: apparent value is not a transfer");
    }
    if policy == ValuePolicy::Reject {
        return BoundaryValue::Rejected("outbe precompile: non-payable address called with value");
    }
    BoundaryValue::Credited(credited)
}
