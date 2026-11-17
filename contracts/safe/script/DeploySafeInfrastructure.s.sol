// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

interface ICreate3Factory {
    function deploy(bytes32 salt, bytes calldata initCode) external payable returns (address deployed);
    function predict(address deployer, bytes32 salt) external view returns (address);
}

/// @dev Deploys the official Safe v1.5.0 singleton, proxy factory and fallback handler on Outbe.
contract DeploySafeInfrastructure is Script {
    string internal constant SAFE_VERSION = "1.5.0";

    function run() external {
        uint256 deployerKey = vm.envUint("DEPLOYER_PK");
        address deployer = vm.addr(deployerKey);
        ICreate3Factory create3 = ICreate3Factory(vm.envAddress("CREATE3_FACTORY_ADDRESS"));

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
        vm.stopBroadcast();

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
}
