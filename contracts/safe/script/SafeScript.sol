// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

interface ISafeProxy {
    function masterCopy() external view returns (address);
}

interface ISafe {
    function setup(
        address[] calldata owners,
        uint256 threshold,
        address to,
        bytes calldata data,
        address fallbackHandler,
        address paymentToken,
        uint256 payment,
        address payable paymentReceiver
    ) external;

    function VERSION() external view returns (string memory);
    function getThreshold() external view returns (uint256);
    function getOwners() external view returns (address[] memory);
}

interface ISafeProxyFactory {
    function proxyCreationCode() external pure returns (bytes memory);

    function createChainSpecificProxyWithNonceL2(address singleton, bytes memory initializer, uint256 saltNonce)
        external
        returns (ISafeProxy proxy);
}

abstract contract SafeScript {
    string internal constant SAFE_VERSION = "1.5.0";

    function _initializer(address[] memory owners, uint256 threshold, address fallbackHandler)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodeCall(
            ISafe.setup, (owners, threshold, address(0), bytes(""), fallbackHandler, address(0), 0, payable(address(0)))
        );
    }

    function _safeAddress(ISafeProxyFactory factory, address singleton, bytes memory initializer, uint256 nonce)
        internal
        view
        returns (address)
    {
        bytes memory deploymentData = abi.encodePacked(factory.proxyCreationCode(), uint256(uint160(singleton)));
        bytes32 salt = keccak256(abi.encodePacked(keccak256(initializer), nonce, block.chainid));
        return address(
            uint160(
                uint256(keccak256(abi.encodePacked(bytes1(0xff), address(factory), salt, keccak256(deploymentData))))
            )
        );
    }

    function _validateConfig(address[] memory owners, uint256 threshold) internal pure {
        require(owners.length != 0, "At least one Safe owner is required");
        require(threshold != 0 && threshold <= owners.length, "Invalid Safe threshold");
        for (uint256 i; i < owners.length; ++i) {
            require(owners[i] != address(0), "Safe owner is zero");
            for (uint256 j; j < i; ++j) {
                require(owners[i] != owners[j], "Safe owners must be unique");
            }
        }
    }

    function _validateDeployment(address safe, address singleton, address[] memory owners, uint256 threshold)
        internal
        view
    {
        require(safe.code.length != 0, "Safe proxy is missing");
        require(ISafeProxy(safe).masterCopy() == singleton, "Safe singleton mismatch");
        require(keccak256(bytes(ISafe(safe).VERSION())) == keccak256(bytes(SAFE_VERSION)), "Safe version mismatch");
        require(ISafe(safe).getThreshold() == threshold, "Safe threshold mismatch");

        address[] memory actualOwners = ISafe(safe).getOwners();
        require(actualOwners.length == owners.length, "Safe owners length mismatch");
        for (uint256 i; i < owners.length; ++i) {
            require(actualOwners[i] == owners[i], "Safe owners mismatch");
        }
    }
}
