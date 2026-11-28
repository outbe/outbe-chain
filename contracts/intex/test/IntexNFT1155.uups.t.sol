// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "./utils/DeployProxy.sol";
import {OpenSeriesLib} from "./utils/OpenSeriesLib.sol";

contract IntexNFT1155UupsTest is Test {
    address internal admin = makeAddr("admin");
    address internal bridger = makeAddr("bridger");
    address internal stranger = makeAddr("stranger");

    IntexNFT1155 internal nft;

    function setUp() public {
        nft = DeployProxy.intexNFT1155(admin, bridger);
    }

    function test_Initialize_SetsTheOwner() public view {
        assertEq(nft.owner(), admin);
    }

    function test_RevertWhen_InitializeCalledTwice() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        nft.initialize(stranger);
    }

    function test_RevertWhen_ImplementationInitialized() public {
        IntexNFT1155 impl = _implementation();
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        impl.initialize(admin);
    }

    function test_RevertWhen_InitializeZeroOwner() public {
        IntexNFT1155 impl = _implementation();
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.ZeroAddress.selector, "owner", address(0)));
        new ERC1967Proxy(address(impl), abi.encodeCall(IntexNFT1155.initialize, (address(0))));
    }

    function test_RevertWhen_UpgradeByNonOwner() public {
        IntexNFT1155 newImpl = _implementation();
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, stranger));
        nft.upgradeToAndCall(address(newImpl), "");
    }

    function test_Upgrade_PreservesStateAndSwapsImplementation() public {
        vm.prank(bridger);
        nft.openSeries(OpenSeriesLib.params(7, 100, 0));
        vm.prank(bridger);
        nft.issueIntex(stranger, 3, OpenSeriesLib.seriesId(7));

        IntexNFT1155 newImpl = _implementation();
        vm.prank(admin);
        nft.upgradeToAndCall(address(newImpl), "");

        bytes32 implSlot = vm.load(address(nft), ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), address(newImpl));
        uint256 intexId = uint256(keccak256(abi.encodePacked(OpenSeriesLib.seriesId(7))));
        assertEq(nft.balanceOf(stranger, intexId), 3);
        assertEq(nft.totalSupply(intexId), 3);
        assertEq(nft.intexAuctionTargetRouter(), bridger);
        assertEq(nft.owner(), admin);
    }

    function _implementation() internal returns (IntexNFT1155) {
        return new IntexNFT1155(bridger, bridger, bridger, bridger, bridger);
    }
}
