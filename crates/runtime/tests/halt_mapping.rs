//! halt-mapping unit coverage for
//! [`outbe_evm::precompiles::map_outbe_precompile_result`].
//!
//! Verifies every variant of `outbe_primitives::error::PrecompileError`
//! routes to the correct evm2 [`PrecompileResult`] form:
//!
//! - `Ok(bytes)` -> `Ok(PrecompileOutput::new(bytes))`
//! - `OutOfGas` -> `Err(PrecompileError::Halt(PrecompileHalt::OutOfGas))`
//! - `Revert(msg)` -> `Err(PrecompileError::Revert(Error(string)-encoded msg))`
//! - `RevertBytes(bytes)` -> `Err(PrecompileError::Revert(bytes))` (no re-encoding)
//! - `BodyReadCorruption(s)` -> `Err(PrecompileError::Revert(Error(string)-encoded reason))`
//! - `WriteProtection` -> `Err(PrecompileError::Halt(Other("state change during static call")))`
//! - VM sub-call failures -> `Err(PrecompileError::Halt(_))`; provider failures -> `Err(PrecompileError::Fatal(_))`
//! - `Unsupported` -> `Err(PrecompileError::Fatal(_))`
//! - `Storage(s)` -> `Err(PrecompileError::Fatal(_))`
//! - `Fatal(s)` -> `Err(PrecompileError::Fatal(_))`

use alloy_primitives::Bytes;
use alloy_sol_types::{Revert, SolError};
use evm2::{
    interpreter::InstrStop,
    precompiles::{PrecompileError as EvmPrecompileError, PrecompileHalt},
};
use outbe_evm::precompiles::map_outbe_precompile_result;
use outbe_primitives::{error::PrecompileError, storage::SubCallError};

#[test]
fn success_carries_output_bytes() {
    let bytes = Bytes::from_static(b"hello");
    let result = map_outbe_precompile_result(Ok(bytes.clone())).expect("ok status");
    assert_eq!(result.bytes(), bytes.as_ref());
}

#[test]
fn out_of_gas_halts_with_oog_status() {
    match map_outbe_precompile_result(Err(PrecompileError::OutOfGas)) {
        Err(EvmPrecompileError::Halt(reason)) => {
            assert!(matches!(reason, PrecompileHalt::OutOfGas));
        }
        other => panic!("expected Halt(OOG), got {other:?}"),
    }
}

#[test]
fn revert_msg_becomes_error_string_bytes() {
    let msg = "explicit revert reason".to_string();
    match map_outbe_precompile_result(Err(PrecompileError::Revert(msg.clone()))) {
        Err(EvmPrecompileError::Revert(bytes)) => {
            // The mapper ABI-encodes the reason as the Solidity-standard `Error(string)`
            // (selector 0x08c379a0 ++ abi.encode(reason)) so ethers/viem/foundry
            // can decode it instead of seeing raw UTF-8 ("invalid data length").
            assert_eq!(bytes, Bytes::from(Revert::from(msg.clone()).abi_encode()));
            assert_eq!(&bytes[..4], &[0x08, 0xc3, 0x79, 0xa0]);
            let decoded = Revert::abi_decode(&bytes).expect("decodes as Error(string)");
            assert_eq!(decoded.reason, msg);
        }
        other => panic!("expected Revert, got {other:?}"),
    }
}

#[test]
fn revert_bytes_preserves_raw_payload() {
    let raw = Bytes::from_static(&[0x01, 0x02, 0xFF, 0x00]);
    match map_outbe_precompile_result(Err(PrecompileError::RevertBytes(raw.clone()))) {
        Err(EvmPrecompileError::Revert(bytes)) => {
            assert_eq!(bytes, raw, "RevertBytes must not re-encode");
        }
        other => panic!("expected Revert, got {other:?}"),
    }
}

#[test]
fn body_read_mismatch_reverts_with_its_reason() {
    let error = PrecompileError::BodyReadCorruption("body commitment mismatch".into());
    let reason = error.to_string();
    match map_outbe_precompile_result(Err(error)) {
        Err(EvmPrecompileError::Revert(bytes)) => {
            assert_eq!(Revert::abi_decode(&bytes).unwrap().reason, reason);
        }
        other => panic!("expected Revert, got {other:?}"),
    }
}

#[test]
fn write_protection_halts_with_static_call_message() {
    match map_outbe_precompile_result(Err(PrecompileError::WriteProtection)) {
        Err(EvmPrecompileError::Halt(reason)) => {
            // The message text is part of the public surface for test
            // assertion. If you rename the literal, update both call sites.
            let msg = format!("{reason:?}");
            assert!(
                msg.contains("state change during static call"),
                "expected static-call write-protection halt message, got {msg}"
            );
            assert!(
                !matches!(reason, PrecompileHalt::OutOfGas),
                "WriteProtection must NOT classify as OOG"
            );
        }
        other => panic!("expected Halt(Other), got {other:?}"),
    }
}

#[test]
fn subcall_error_is_fatal_with_variant_info() {
    let result =
        map_outbe_precompile_result(Err(PrecompileError::SubCall(SubCallError::NotAvailable)));
    match result {
        Err(EvmPrecompileError::Fatal(err)) => {
            let msg = err.to_string();
            assert!(
                msg.contains("sub-call"),
                "fatal message must mention sub-call, got {msg}"
            );
            assert!(
                msg.contains("not available"),
                "fatal message must surface the variant, got {msg}"
            );
        }
        other => panic!("expected Fatal, got {other:?}"),
    }
}

#[test]
fn unsupported_is_fatal_with_explanatory_message() {
    let result = map_outbe_precompile_result(Err(PrecompileError::Unsupported));
    match result {
        Err(EvmPrecompileError::Fatal(err)) => {
            let msg = err.to_string();
            assert!(
                msg.to_ascii_lowercase().contains("unsupported"),
                "fatal message must mention unsupported, got {msg}"
            );
        }
        other => panic!("expected Fatal, got {other:?}"),
    }
}

#[test]
fn storage_error_falls_back_to_fatal() {
    let result =
        map_outbe_precompile_result(Err(PrecompileError::Storage("db read failed".into())));
    match result {
        Err(EvmPrecompileError::Fatal(err)) => {
            let msg = err.to_string();
            assert!(
                msg.contains("db read failed"),
                "expected message echo, got {msg}"
            );
        }
        other => panic!("expected Fatal, got {other:?}"),
    }
}

#[test]
fn fatal_passes_through_to_fatal() {
    let result = map_outbe_precompile_result(Err(PrecompileError::Fatal("unrecoverable".into())));
    match result {
        Err(EvmPrecompileError::Fatal(err)) => {
            let msg = err.to_string();
            assert!(
                msg.contains("unrecoverable"),
                "expected original payload to survive, got {msg}"
            );
        }
        other => panic!("expected Fatal, got {other:?}"),
    }
}

#[test]
fn child_vm_failures_use_standard_halt_results_without_revert_bytes() {
    for error in [
        SubCallError::OutOfGas,
        SubCallError::ParentOutOfGas,
        SubCallError::DepthLimitExceeded,
        SubCallError::StaticContextViolation,
        SubCallError::StateChangeDuringStaticCall,
        SubCallError::InvalidTarget,
        SubCallError::NotActivated,
        SubCallError::EvmHalt(InstrStop::InvalidFEOpcode),
        SubCallError::EvmHalt(InstrStop::OutOfGas),
        SubCallError::EvmHalt(InstrStop::OutOfFunds),
    ] {
        let expected_oog = matches!(
            error,
            SubCallError::OutOfGas
                | SubCallError::ParentOutOfGas
                | SubCallError::EvmHalt(InstrStop::OutOfGas)
        );
        match map_outbe_precompile_result(Err(error.into())) {
            Err(EvmPrecompileError::Halt(reason)) => {
                assert_eq!(matches!(reason, PrecompileHalt::OutOfGas), expected_oog)
            }
            other => panic!("expected standard Halt, got {other:?}"),
        }
    }
}

#[test]
fn subcall_provider_failures_stay_fatal() {
    for error in [
        SubCallError::DatabaseError("db unavailable".into()),
        SubCallError::ProviderBorrowed,
        SubCallError::NotAvailable,
        SubCallError::Fatal("driver failed".into()),
    ] {
        assert!(matches!(
            map_outbe_precompile_result(Err(error.into())),
            Err(EvmPrecompileError::Fatal(_))
        ));
    }
}
