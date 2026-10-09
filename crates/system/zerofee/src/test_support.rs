//! Shared fixtures for the zero-fee hook tests.

use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::precompile::IHyperlaneController;
use outbe_ocomp_protocol::transaction_call::TransactionCallFields;
use outbe_primitives::{error::PrecompileError, storage::StorageHandle};
use outbe_validatorset::{
    contract::ValidatorSet, delegation::ValidatorDelegateRole, StakeProjection,
};

use crate::hooks::ZeroFeeTransaction;

/// Owner of the validator set configuration in hook tests.
const VALIDATOR_SET_OWNER: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");

/// A zero-fee transaction from `signer` to `to` that pays the protocol minimum
/// fee cap and no priority fee.
pub(crate) fn hook_tx(signer: Address, to: Address, input: &[u8]) -> ZeroFeeTransaction<'_> {
    ZeroFeeTransaction {
        signer,
        call: TransactionCallFields {
            to: Some(to),
            value: U256::ZERO,
            input,
            gas_limit: 1_000_000,
            max_fee_per_gas: u128::from(MIN_PROTOCOL_BASE_FEE),
            max_priority_fee_per_gas: Some(0),
        },
    }
}

/// Valid `HyperlaneController.submitCheckpoint` calldata.
pub(crate) fn checkpoint_calldata() -> Bytes {
    IHyperlaneController::submitCheckpointCall {
        domain: 54_322_345,
        root: B256::repeat_byte(1),
        index: 7,
        messageId: B256::repeat_byte(2),
        signature: Bytes::from(vec![3u8; 65]),
    }
    .abi_encode()
    .into()
}

/// Registers and activates `validator` as the only validator. Then it names
/// `delegate` as the validator delegate for `role`.
pub(crate) fn activate_validator_with_delegate(
    storage: StorageHandle<'_>,
    validator: Address,
    role: ValidatorDelegateRole,
    delegate: Address,
) -> Result<(), PrecompileError> {
    let mut validators = ValidatorSet::new(storage);
    validators.config_owner.write(VALIDATOR_SET_OWNER)?;
    validators.config_max_validators.write(1)?;
    validators.test_register_validator_without_pop(validator, &[1; 48])?;
    validators.test_activate_validator_canonically(
        validator,
        StakeProjection::new(U256::from(1), None),
        U256::from(1),
    )?;
    validators.set_delegate(validator, role, delegate)
}
