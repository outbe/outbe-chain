// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";

import {CreateX} from "../../script/0_DeployCreateX.s.sol";
import {RouteSpec} from "../../script/routes/BaseRoute.sol";
import {DeployAll} from "../../script/DeployAll.s.sol";

contract AddressHarness is DeployAll {
    function exposedSaltHash(string memory label, string memory salt, address deployer)
        external
        pure
        returns (bytes32)
    {
        return _saltHash(label, salt, deployer);
    }

    function exposedTokenAddress(address createX, string memory salt, RouteSpec memory spec)
        external
        view
        returns (address)
    {
        return _tokenAddress(createX, salt, spec);
    }

    function exposedBridgeAddress(address createX, string memory salt, RouteSpec memory spec)
        external
        view
        returns (address)
    {
        return _bridgeAddress(createX, salt, spec);
    }

    function _deployer() internal view override returns (address) {
        return address(this);
    }
}

/// @dev Pins the CREATE3 derivation for every route. Both addresses are live on three chains, so any change to the
///      salt labels or to the hashing formula silently relocates deployed contracts.
contract RouteAddressesTest is Test {
    string internal constant SALT = "TEST_V1";
    address internal constant PINNED_DEPLOYER = address(0xA11C);

    AddressHarness internal deploy;
    CreateX internal createX;

    function setUp() public {
        vm.setEnv("EXTERNAL_CHAIN_ID", "11155111");
        vm.setEnv("OUTBE_CHAIN_ID", "54322345");
        vm.setEnv("DEPLOYER_PK", "0xA11CE");

        deploy = new AddressHarness();
        createX = new CreateX();
    }

    /// @dev A literal snapshot, unlike the per-route checks below: those restate the derivation, so a change made in
    ///      both the code and the expectation would slip through.
    function test_SaltHashes_MatchTheDeployedSnapshot() public view {
        assertEq(
            deploy.exposedSaltHash("USDT", SALT, PINNED_DEPLOYER),
            0x1173d2a590a52ef37d924756bb4c85abc3cf66fb5c380f5e6721613d0fdabf45,
            "USDT"
        );
        assertEq(
            deploy.exposedSaltHash("USDTBridge", SALT, PINNED_DEPLOYER),
            0xb143a9690498f901089dfd28c7b240d1a6d4315fb815760c693d990b86de19ed,
            "USDTBridge"
        );
        assertEq(
            deploy.exposedSaltHash("USDC", SALT, PINNED_DEPLOYER),
            0x5f5f6c1a85f7fda8ba03eb540bbc9e8ad571c3b9f0e2b5ff7c1a67a58f768230,
            "USDC"
        );
        assertEq(
            deploy.exposedSaltHash("USDCBridge", SALT, PINNED_DEPLOYER),
            0xf1aa1dd831891dcb9c628fc9086a9950422190b29e87680da835275446a905e6,
            "USDCBridge"
        );
        assertEq(
            deploy.exposedSaltHash("WCOEN", SALT, PINNED_DEPLOYER),
            0xf1035dc456582653c95fa609764458b1683c104cd5a772aa077ca201a2134733,
            "WCOEN"
        );
        assertEq(
            deploy.exposedSaltHash("WCOENBridge", SALT, PINNED_DEPLOYER),
            0xc8e1e21df01142d4883d8a7cce661a360c4c324096fba5ede37cd8522eb96727,
            "WCOENBridge"
        );
    }

    function test_UsdtRoute_DerivesFromItsLabels() public {
        _assertRoute(deploy.routeByLabel("USDT").spec, "USDT");
    }

    function test_UsdcRoute_DerivesFromItsLabels() public {
        _assertRoute(deploy.routeByLabel("USDC").spec, "USDC");
    }

    function test_WcoenRoute_DerivesFromItsLabels() public {
        _assertRoute(deploy.routeByLabel("WCOEN").spec, "WCOEN");
    }

    /// @dev The bridge label is the token label plus `Bridge`; dropping the stored `bridgeLabel` must keep that exact
    ///      string, since it is what the live deployments were salted with.
    function _assertRoute(RouteSpec memory spec, string memory label) internal view {
        assertEq(spec.tokenLabel, label, "token label");

        assertEq(
            deploy.exposedTokenAddress(address(createX), SALT, spec),
            createX.computeCreate3Address(keccak256(abi.encodePacked(label, SALT, address(deploy)))),
            "token address"
        );

        assertEq(
            deploy.exposedBridgeAddress(address(createX), SALT, spec),
            createX.computeCreate3Address(
                keccak256(abi.encodePacked(string.concat(label, "Bridge"), SALT, address(deploy)))
            ),
            "bridge address"
        );
    }
}
