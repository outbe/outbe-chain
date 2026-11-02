// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// Test-only ISM and ICA router for the live HyperlaneController precompile. Their
/// owner is the CrosschainGov precompile.
contract MockHyperlaneIsm {
    address public owner;
    address public pendingOwner;
    address[] private validators;
    uint8 private threshold;

    constructor() {
        owner = msg.sender;
    }

    function transferOwnership(address next) external {
        require(msg.sender == owner, "owner");
        pendingOwner = next;
    }

    function acceptOwnership() external {
        require(msg.sender == pendingOwner, "pending owner");
        owner = msg.sender;
        pendingOwner = address(0);
    }

    function setValidatorsAndThreshold(address[] calldata next, uint8 nextThreshold) external {
        require(msg.sender == owner, "owner");
        delete validators;
        for (uint256 i; i < next.length; ++i) validators.push(next[i]);
        threshold = nextThreshold;
    }

    function validatorsAndThreshold(bytes calldata)
        external
        view
        returns (address[] memory, uint8)
    {
        return (validators, threshold);
    }
}

contract MockHyperlaneIcaRouter {
    address public immutable owner;

    constructor(address owner_) {
        owner = owner_;
    }
}
