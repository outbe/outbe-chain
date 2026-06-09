// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";

/// @dev EIP-170 runtime-size guard for IntexNFT1155.
///
/// IntexNFT1155 sits within a thin margin of the 24,576-byte EIP-170 runtime limit (its
/// metadata/SVG rendering is already split into the linked IntexMetadata library to claw back
/// space). Critically, `forge test` does NOT enforce EIP-170 on deploy — an over-limit contract
/// still deploys and runs in the test EVM, so a size regression passes the whole suite and only
/// surfaces in `forge build --sizes`. This test promotes the limit to a first-class assertion:
/// it deploys the contract and measures the real runtime bytecode length, failing in `forge test`
/// the moment a change makes the contract undeployable on a real EIP-170 chain.
contract IntexNFT1155SizeTest is Test {
    /// @notice EIP-170 maximum contract runtime bytecode size, in bytes.
    uint256 internal constant EIP170_LIMIT = 24_576;

    function test_IntexNFT1155_RuntimeSize_WithinEIP170() public {
        // Non-zero constructor args; the linked IntexMetadata library is resolved by forge.
        IntexNFT1155 nft = new IntexNFT1155(address(0xA11CE), address(0xB1D6E));
        uint256 size = address(nft).code.length;
        assertLe(size, EIP170_LIMIT, "IntexNFT1155 runtime bytecode exceeds the EIP-170 limit");
    }
}
