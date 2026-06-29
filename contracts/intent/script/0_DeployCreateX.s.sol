// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

/// @dev Minimal CREATE3 factory. Deployed address depends only on (factory, salt),
///      not on the contract's bytecode or constructor arguments.
/// @notice CREATE3 = CREATE2(fixed proxy) + CREATE(nonce=0) from that proxy.
contract CreateX {
    event Deployed(bytes32 indexed salt, address indexed deployed);

    /// @dev Proxy bytecode that forwards calldata to CREATE:
    ///      CALLDATASIZE PUSH0 PUSH0 CALLDATACOPY CALLDATASIZE PUSH0 PUSH0 CREATE PUSH0 MSTORE PUSH1(20) PUSH1(12)
    /// RETURN
    bytes internal constant _PROXY_BYTECODE = hex"67363d3d37363d34f03d5260086018f3";

    function deployCreate3(bytes32 salt, bytes memory initCode) external payable returns (address deployed) {
        address proxy;
        bytes memory proxyCode = _PROXY_BYTECODE;
        assembly {
            proxy := create2(0, add(proxyCode, 0x20), mload(proxyCode), salt)
        }
        require(proxy != address(0), "CREATE3: proxy failed");

        (bool ok,) = proxy.call(initCode);
        require(ok, "CREATE3: deploy failed");

        deployed = _computeDeployed(proxy);
        require(deployed.code.length > 0, "CREATE3: no code");

        emit Deployed(salt, deployed);
    }

    function computeCreate3Address(bytes32 salt) external view returns (address) {
        address proxy = address(
            uint160(uint256(keccak256(abi.encodePacked(bytes1(0xff), address(this), salt, keccak256(_PROXY_BYTECODE)))))
        );
        return _computeDeployed(proxy);
    }

    function _computeDeployed(address proxy) internal pure returns (address) {
        return address(uint160(uint256(keccak256(abi.encodePacked(bytes1(0xd6), bytes1(0x94), proxy, bytes1(0x01))))));
    }
}

/// @dev Deploys the CreateX factory at a deterministic address using CREATE2.
///
/// Required env vars:
///   DEPLOYER_PK    — deployer private key
///   CONTRACT_SALT  — salt string for deterministic deployment
contract DeployCreateXDeterministic is Script {
    function run() public virtual {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envString("CONTRACT_SALT");

        vm.startBroadcast(deployerPrivateKey);
        address createx = deployCreateX(salt);
        vm.stopBroadcast();

        console2.log("CreateX deployed at:", createx);
    }

    function deployCreateX(string memory salt) public returns (address) {
        bytes32 saltHash = keccak256(abi.encode(salt));

        // Idempotent: the address is deterministic (CREATE2 via the default factory), so if a CreateX is already
        // deployed at it, reuse that one instead of reverting on the collision.
        address predicted = vm.computeCreate2Address(saltHash, keccak256(type(CreateX).creationCode));
        if (predicted.code.length != 0) {
            console2.log("CreateX already deployed, reusing:", predicted);
            return predicted;
        }

        CreateX createx = new CreateX{salt: saltHash}();
        return address(createx);
    }
}
