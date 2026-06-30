// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

/**
 * @dev Minimal encoder for Hyperlane's StandardHookMetadata, vendored locally so {HyperlaneGatewayAdapter} can
 * request a per-message destination gas limit from the Interchain Gas Paymaster without depending on the Hyperlane
 * SDK. The byte layout matches Hyperlane core (variant, msgValue, gasLimit, refundAddress), so the real IGP hook
 * parses it.
 */
library StandardHookMetadata {
    /// @dev Metadata variant tag understood by Hyperlane hooks.
    uint16 private constant VARIANT = 1;

    /// @dev Encodes metadata overriding only the destination execution gas limit (no extra forwarded value).
    function overrideGasLimit(uint256 gasLimit, address refundAddress) internal pure returns (bytes memory) {
        return abi.encodePacked(VARIANT, uint256(0), gasLimit, refundAddress);
    }
}
