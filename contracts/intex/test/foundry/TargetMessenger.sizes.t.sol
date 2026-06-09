// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";

/// @dev EIP-170 runtime-size guard for TargetMessenger.
///
/// TargetMessenger is the tightest cross-chain contract (~412 B under the 24,576-byte EIP-170
/// runtime limit) and the second-tightest in the system after IntexNFT1155. `forge test` does NOT
/// enforce EIP-170 on deploy, so a size regression passes the whole suite and only surfaces in
/// `forge build --sizes`. This test promotes the limit to a first-class assertion: it reads the
/// compiled runtime bytecode from the build artifact (so no LayerZero endpoint wiring is needed)
/// and fails the moment a change makes the contract undeployable on a real EIP-170 chain.
contract TargetMessengerSizeTest is Test {
    /// @notice EIP-170 maximum contract runtime bytecode size, in bytes.
    uint256 internal constant EIP170_LIMIT = 24_576;

    function test_TargetMessenger_RuntimeSize_WithinEIP170() public {
        uint256 size = vm.getDeployedCode("TargetMessenger.sol:TargetMessenger").length;
        assertLe(size, EIP170_LIMIT, "TargetMessenger runtime bytecode exceeds the EIP-170 limit");
    }
}
