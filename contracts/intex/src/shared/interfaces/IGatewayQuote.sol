// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/**
 * @dev Fee-quoting extension for ERC-7786 gateways. It is not part of the ERC-7786 core, which
 * leaves fee discovery to the transport. It mirrors the interface that the `crosschain` hub's
 * `ERC7786Bridge` exposes, so the intex routers can ask the bridge for the native fee before
 * sending. This file is vendored because OpenZeppelin does not ship it.
 */
interface IGatewayQuote {
    /// @dev Native fee required to deliver `payload` to `recipient` (an ERC-7930 interoperable
    /// address).
    function quote(bytes calldata recipient, bytes calldata payload) external view returns (uint256 nativeFee);

    /// @dev As above, but honoring `attributes` (e.g. a per-message execution gas limit) so the
    /// estimate matches {IERC7786GatewaySource-sendMessage}.
    function quote(bytes calldata recipient, bytes calldata payload, bytes[] calldata attributes)
        external
        view
        returns (uint256 nativeFee);
}
