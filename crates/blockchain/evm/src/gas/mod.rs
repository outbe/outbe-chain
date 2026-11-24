//! Sub-call gas accounting module.
//!
//! This module contains [`SubcallGasMeter`], a thin wrapper over
//! [`evm2::interpreter::Gas`]. The wrapper exposes the API that the outbe sub-call driver
//! ([`crate::sub_call`]) needs.
//!
//! ## Why a separate type
//!
//! The driver creates and owns one gas meter for each sub-call frame. With this meter, the
//! driver can:
//! 1. enforce the EIP-150 forward-cap independently of the outer interpreter's
//!    [`Gas`](evm2::interpreter::Gas).
//! 2. capture the settlement triple:
//!    - Success: erase_cost + record_refund + add_state_gas_spent.
//!    - Revert: erase_cost only.
//!    - Halt: outer meter unchanged.
//! 3. propagate `reservoir` / `state_gas_spent` back to the outer meter through
//!    `merge_child_gas`.
//!
//! ## Mirror discipline
//!
//! Every method delegates to the inner [`evm2::interpreter::Gas`] instance. Thus the
//! semantics are byte-equal by construction.

pub mod subcall_meter;

pub use subcall_meter::SubcallGasMeter;
