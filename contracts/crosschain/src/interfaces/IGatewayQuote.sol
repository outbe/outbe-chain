// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

/**
 * @dev Fee-quoting extension for ERC-7786 gateways.
 *
 * ERC-7786 itself does not standardize fee discovery (it is transport-specific). This minimal
 * interface lets a facade expose a uniform `quote` to applications. The facade delegates the actual
 * estimate to the active gateway adapter, which knows the underlying protocol's pricing.
 */
interface IGatewayQuote {
    /// @dev Native fee required to deliver `payload` to `recipient` (an ERC-7930 interoperable address).
    function quote(bytes calldata recipient, bytes calldata payload) external view returns (uint256 nativeFee);

    /// @dev As above, but honoring `attributes` (e.g. a per-message execution gas limit) so the
    /// estimate matches {IERC7786GatewaySource-sendMessage}.
    function quote(bytes calldata recipient, bytes calldata payload, bytes[] calldata attributes)
        external
        view
        returns (uint256 nativeFee);
}
