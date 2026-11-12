//! Shared execution-test fixture. Scenario assertions stay in their callers.

use alloy_primitives::{Address, Bytes};

pub(super) fn borrow_code(opcode: u8, target: Address) -> Bytes {
    let mut code = vec![
        0x36, // CALLDATASIZE          size
        0x60, 0x00, // PUSH1 0         offset
        0x60, 0x00, // PUSH1 0         destOffset
        0x37, // CALLDATACOPY
        0x60, 0x00, // PUSH1 0         retLength
        0x60, 0x00, // PUSH1 0         retOffset
        0x36, // CALLDATASIZE          argsLength
        0x60, 0x00, // PUSH1 0         argsOffset
    ];
    if opcode == 0xf2 {
        code.push(0x34); // CALLVALUE   value
    }
    code.push(0x73); // PUSH20         address
    code.extend_from_slice(target.as_slice());
    code.push(0x5a); // GAS
    code.push(opcode);
    code.extend_from_slice(&[
        0x50, // POP                   drop the success flag
        0x3d, // RETURNDATASIZE        size
        0x60, 0x00, // PUSH1 0         offset
        0x60, 0x00, // PUSH1 0         destOffset
        0x3e, // RETURNDATACOPY
        0x3d, // RETURNDATASIZE        size
        0x60, 0x00, // PUSH1 0         offset
        0xf3, // RETURN                bubble the inner frame's returndata up
    ]);
    Bytes::from(code)
}
