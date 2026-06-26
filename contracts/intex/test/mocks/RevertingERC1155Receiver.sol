// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @notice ERC-1155 receiver whose hook reverts while `reject` is set, then accepts once toggled off.
///         Simulates a contract recipient that blocks (and later unblocks) its own issuance mint.
contract RevertingERC1155Receiver {
    error Rejected();

    bool public reject = true;

    function setReject(bool value) external {
        reject = value;
    }

    function onERC1155Received(address, address, uint256, uint256, bytes calldata) external view returns (bytes4) {
        if (reject) revert Rejected();
        return this.onERC1155Received.selector;
    }

    function onERC1155BatchReceived(address, address, uint256[] calldata, uint256[] calldata, bytes calldata)
        external
        view
        returns (bytes4)
    {
        if (reject) revert Rejected();
        return this.onERC1155BatchReceived.selector;
    }
}
