// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// ERC-1967 proxy and UUPS implementation for the CrosschainGov `upgradeContract` test. Compile with solc 0.8.30,
/// optimizer 200, via-ir, Prague, metadata hash none, CBOR disabled. `UupsProxy.hex` and `UupsCounter.hex` are the
/// runtime code of the two contracts.

/// Runs every call with the code of the implementation in the ERC-1967 slot, in the storage of the proxy.
contract UupsProxy {
    bytes32 internal constant IMPLEMENTATION_SLOT = 0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc;

    fallback() external payable {
        assembly {
            let implementation := sload(IMPLEMENTATION_SLOT)
            calldatacopy(0, 0, calldatasize())
            let ok := delegatecall(gas(), implementation, 0, calldatasize(), 0, 0)
            returndatacopy(0, 0, returndatasize())
            switch ok
            case 0 { revert(0, returndatasize()) }
            default { return(0, returndatasize()) }
        }
    }
}

/// UUPS implementation with the OpenZeppelin `upgradeToAndCall` checks: the owner only, and an ERC-1822
/// `proxiableUUID` on the new implementation. `owner` and `count` are the state of the proxy.
contract UupsCounter {
    bytes32 internal constant IMPLEMENTATION_SLOT = 0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc;

    error OwnableUnauthorizedAccount(address account);
    error UUPSUnsupportedProxiableUUID(bytes32 slot);

    address public owner;
    uint256 public count;

    function increment() external {
        count += 1;
    }

    function implementation() external view returns (address result) {
        assembly {
            result := sload(IMPLEMENTATION_SLOT)
        }
    }

    function proxiableUUID() external pure returns (bytes32) {
        return IMPLEMENTATION_SLOT;
    }

    function upgradeToAndCall(address newImplementation, bytes calldata data) external payable {
        if (msg.sender != owner) revert OwnableUnauthorizedAccount(msg.sender);
        bytes32 slot = UupsCounter(newImplementation).proxiableUUID();
        if (slot != IMPLEMENTATION_SLOT) revert UUPSUnsupportedProxiableUUID(slot);
        assembly {
            sstore(IMPLEMENTATION_SLOT, newImplementation)
        }
        if (data.length > 0) {
            (bool ok, bytes memory ret) = newImplementation.delegatecall(data);
            if (!ok) {
                assembly {
                    revert(add(ret, 32), mload(ret))
                }
            }
        }
    }
}
