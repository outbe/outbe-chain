// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {Create3Factory} from "@contracts/deploy/Create3Factory.sol";
import {Create3Deploy} from "./Create3Deploy.sol";

/// @title BaseScript
/// @author Outbe
/// @notice Shared deployment plumbing: a deterministic CREATE3 factory plus salt-versioned,
///         idempotent UUPS proxy deployment through it.
/// @dev The factory is deployed once per chain through the canonical CREATE2 deployer
///      (`CREATE2_FACTORY`, inherited from `Script`) at a pinned salt, so it has the same address
///      on every chain. Proxy addresses then depend only on `(factory, deployer, salt)`.
abstract contract BaseScript is Script {
    /// @notice Bump to move every deployment to a fresh set of addresses.
    string internal constant SALT_VERSION = "v1.0.0";

    /// @notice Pinned salt for the CREATE3 factory itself.
    bytes32 internal constant FACTORY_SALT = keccak256("outbe-intex:Create3Factory:v1.0.0");

    /// @notice Deploy the CREATE3 factory deterministically if absent, else return the existing one.
    /// @return factory The CREATE3 factory at its deterministic, cross-chain-identical address.
    function ensureCreate3Factory() public returns (Create3Factory factory) {
        bytes memory initCode = type(Create3Factory).creationCode;
        address predicted = vm.computeCreate2Address(FACTORY_SALT, keccak256(initCode), CREATE2_FACTORY);
        if (predicted.code.length == 0) {
            (bool ok,) = CREATE2_FACTORY.call(abi.encodePacked(FACTORY_SALT, initCode));
            require(ok, "Create3Factory deploy failed");
            require(predicted.code.length != 0, "Create3Factory missing after deploy");
        }
        return Create3Factory(predicted);
    }

    /// @notice Predict a proxy address without deploying.
    function predictProxy(Create3Factory factory, address deployer, string memory prefix)
        public
        view
        returns (address)
    {
        return Create3Deploy.predictProxy(factory, deployer, prefix, SALT_VERSION);
    }

    /// @notice Deploy `impl` behind a UUPS proxy through `factory`, idempotently.
    function deployProxy(
        Create3Factory factory,
        address deployer,
        string memory prefix,
        address impl,
        bytes memory initData
    ) public returns (address) {
        return Create3Deploy.deployProxy(factory, deployer, prefix, SALT_VERSION, impl, initData);
    }
}
