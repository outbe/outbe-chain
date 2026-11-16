// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

interface ICreate3Factory {
    function deploy(bytes32 salt, bytes calldata initCode) external payable returns (address deployed);
    function predict(address deployer, bytes32 salt) external view returns (address);
}

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

/// @dev Deploys the official Safe v1.5.0 contracts and one chain-specific Safe account on Outbe.
///      Re-running with the same owners and threshold returns the existing account.
contract DeploySafe is Script {
    string internal constant SAFE_VERSION = "1.5.0";
    uint256 internal constant SAFE_NONCE = 0;

    function run() external {
        uint256 deployerKey = vm.envUint("DEPLOYER_PK");
        address deployer = vm.addr(deployerKey);
        ICreate3Factory create3 = ICreate3Factory(vm.envAddress("CREATE3_FACTORY_ADDRESS"));
        address[] memory owners = vm.envAddress("SAFE_OWNERS", ",");
        uint256 threshold = vm.envUint("SAFE_THRESHOLD");

        _validateConfig(owners, threshold);
        require(block.chainid == vm.envUint("OUTBE_CHAIN_ID"), "Unexpected chain id");
        require(address(create3).code.length != 0, "CREATE3 factory is missing");

        vm.startBroadcast(deployerKey);
        address singleton = _deploy(create3, deployer, "SafeL2", vm.getCode("SafeL2.sol:SafeL2"));
        address proxyFactory =
            _deploy(create3, deployer, "SafeProxyFactory", vm.getCode("SafeProxyFactory.sol:SafeProxyFactory"));
        address fallbackHandler = _deploy(
            create3,
            deployer,
            "CompatibilityFallbackHandler",
            vm.getCode("CompatibilityFallbackHandler.sol:CompatibilityFallbackHandler")
        );

        bytes memory initializer = abi.encodeCall(
            ISafe.setup, (owners, threshold, address(0), bytes(""), fallbackHandler, address(0), 0, payable(address(0)))
        );
        address safe = _safeAddress(ISafeProxyFactory(proxyFactory), singleton, initializer);
        if (safe.code.length == 0) {
            safe = address(
                ISafeProxyFactory(proxyFactory).createChainSpecificProxyWithNonceL2(singleton, initializer, SAFE_NONCE)
            );
        }
        vm.stopBroadcast();

        _validateDeployment(safe, singleton, owners, threshold);

        console2.log("SAFE_ADDRESS:", safe);
        console2.log("SAFE_SINGLETON:", singleton);
        console2.log("SAFE_PROXY_FACTORY:", proxyFactory);
        console2.log("SAFE_FALLBACK_HANDLER:", fallbackHandler);
    }

    function _deploy(ICreate3Factory factory, address deployer, string memory name, bytes memory initCode)
        internal
        returns (address deployed)
    {
        bytes32 salt = keccak256(bytes(string.concat("outbe:safe:", name, ":v", SAFE_VERSION)));
        deployed = factory.predict(deployer, salt);
        if (deployed.code.length == 0) deployed = factory.deploy(salt, initCode);
        require(deployed.code.length != 0, "Safe dependency deployment failed");
    }

    function _safeAddress(ISafeProxyFactory factory, address singleton, bytes memory initializer)
        internal
        view
        returns (address)
    {
        bytes memory deploymentData = abi.encodePacked(factory.proxyCreationCode(), uint256(uint160(singleton)));
        bytes32 salt = keccak256(abi.encodePacked(keccak256(initializer), SAFE_NONCE, block.chainid));
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
