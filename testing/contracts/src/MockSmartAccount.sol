// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @notice E2E account: the user and CCA have identical, unrestricted call access.
contract MockSmartAccount {
    address public immutable user;
    address public immutable cca;

    constructor(address user_, address cca_) {
        require(user_ != address(0) && cca_ != address(0), "ZERO_ACTOR");
        user = user_;
        cca = cca_;
    }

    receive() external payable {}

    function execute(address target, uint256 value, bytes calldata data) external payable returns (bytes memory) {
        require(msg.sender == user || msg.sender == cca, "UNAUTHORIZED");
        (bool ok, bytes memory result) = target.call{value: value}(data);
        if (!ok) {
            assembly ("memory-safe") {
                revert(add(result, 32), mload(result))
            }
        }
        return result;
    }
}
